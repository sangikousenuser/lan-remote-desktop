#import <AppKit/AppKit.h>
#import <Carbon/Carbon.h>
#import <AVFoundation/AVFoundation.h>
#import <VideoToolbox/VideoToolbox.h>
#import <MetalKit/MetalKit.h>
#import <CoreImage/CoreImage.h>
#include <stdatomic.h>
#include <math.h>
#include "audio.inc"
extern char *lrd_interfaces_json(void);
extern void lrd_free_string(char *);
extern void lrd_discover(uint32_t);
extern void lrd_connect(uint32_t, const char *);
extern void lrd_set_transport(uint8_t);
extern void lrd_disconnect(void);
extern bool lrd_input_json(const uint8_t *, size_t);
extern void lrd_send_file(const char *);
extern void lrd_cancel_files(void);
extern bool lrd_clipboard_send(uint8_t,const uint8_t *,size_t);
extern bool lrd_control_json(const uint8_t *,size_t);
extern void lrd_startup_ready(void);
extern char *lrd_resource_statistics_json(void);
extern void lrd_record_metrics_json(const uint8_t *,size_t);
extern char *lrd_choose_display_json(const uint8_t *,size_t);
extern bool lrd_export_numeric_logs(const char *);


// This bridge owns every CF/ObjC object. No borrowed image pointer crosses into Rust.
@interface LRDDecoder : NSObject {
@public
    VTDecompressionSessionRef session;
    CVPixelBufferRef latest;
    NSLock *lock;
    atomic_uint_fast64_t decoded;
    atomic_uint_fast64_t failures;
    atomic_uint_fast64_t presented;
    atomic_uint_fast64_t generation;
    atomic_uint_fast64_t presentedGeneration;
    atomic_bool hardware;
    atomic_uint_fast64_t decodeMicros;
    atomic_uint_fast64_t renderMicros;
    atomic_uint_fast64_t gpuMicros;
    atomic_uint_fast64_t gpuFrames;
    NSData *networkSPS;
    NSData *networkPPS;
    CMVideoFormatDescriptionRef networkFormat;

}
@property(strong) AVAssetReader *reader;
@property(strong) AVAssetReaderTrackOutput *output;
@property(strong) NSURL *url;
@property(copy) NSString *error;
@property BOOL softwareOnly;
@property unsigned int decodeFailureStreak;
- (BOOL)open:(NSURL *)url;
- (BOOL)step;
- (BOOL)decodeSample:(CMSampleBufferRef)sample;
- (BOOL)feedAnnexB:(NSData *)data;

- (CVPixelBufferRef)copyLatest:(uint64_t *)frameGeneration CF_RETURNS_RETAINED;
@end

static void decodedFrame(void *context, void *source, OSStatus status, VTDecodeInfoFlags flags,
                         CVImageBufferRef image, CMTime pts, CMTime duration) {
    (void)source; (void)flags; (void)pts; (void)duration;
    LRDDecoder *decoder = (__bridge LRDDecoder *)context;
    if (status != noErr || !image) { atomic_fetch_add(&decoder->failures, 1); return; }
    [decoder->lock lock];
    if (decoder->latest) CVPixelBufferRelease(decoder->latest);
    decoder->latest = CVPixelBufferRetain(image);
    atomic_fetch_add(&decoder->generation, 1);
    [decoder->lock unlock];
    atomic_fetch_add(&decoder->decoded, 1);
}

@implementation LRDDecoder
- (instancetype)init {
    self = [super init];
    if (self) {
        lock = [NSLock new];
        atomic_init(&decoded, 0); atomic_init(&failures, 0); atomic_init(&presented, 0);
        atomic_init(&generation, 0); atomic_init(&presentedGeneration, 0); atomic_init(&hardware, false);
        atomic_init(&decodeMicros,0);atomic_init(&renderMicros,0);atomic_init(&gpuMicros,0);atomic_init(&gpuFrames,0);
    }
    return self;
}
- (void)dealloc {
    if (session) { VTDecompressionSessionInvalidate(session); CFRelease(session); }
    if (latest) CVPixelBufferRelease(latest);
    if (networkFormat) CFRelease(networkFormat);
}
- (BOOL)open:(NSURL *)url {
    self.url = url;
    AVURLAsset *asset = [AVURLAsset URLAssetWithURL:url options:nil];
    AVAssetTrack *track = [[asset tracksWithMediaType:AVMediaTypeVideo] firstObject];
    if (!track) { self.error = @"H.264映像トラックが見つかりません。"; return NO; }
    NSError *error = nil;
    self.reader = [[AVAssetReader alloc] initWithAsset:asset error:&error];
    self.output = [[AVAssetReaderTrackOutput alloc] initWithTrack:track outputSettings:nil];
    self.output.alwaysCopiesSampleData = NO;
    if (!self.reader || ![self.reader canAddOutput:self.output]) {
        self.error = error.localizedDescription ?: @"映像ファイルを開けません。"; return NO;
    }
    [self.reader addOutput:self.output];
    if (![self.reader startReading]) { self.error = self.reader.error.localizedDescription; return NO; }
    return YES;
}
- (BOOL)step {
    CMSampleBufferRef sample = [self.output copyNextSampleBuffer];
    if (!sample) {
        if (self.reader.status == AVAssetReaderStatusFailed) self.error = self.reader.error.localizedDescription;
        return NO;
    }
    return [self decodeSample:sample];
}
// Consumes the caller-owned sample even on failure.
- (BOOL)decodeSample:(CMSampleBufferRef)sample {
    CMFormatDescriptionRef format = CMSampleBufferGetFormatDescription(sample);
    if (!format && CMSampleBufferGetNumSamples(sample) == 0) { CFRelease(sample); return YES; }
    if (!format || CMFormatDescriptionGetMediaSubType(format) != kCMVideoCodecType_H264) {
        FourCharCode type = format ? CMFormatDescriptionGetMediaSubType(format) : 0;
        self.error = [NSString stringWithFormat:@"検証対象はH.264のみです (format=%08x)。", (unsigned)type]; CFRelease(sample); return NO;
    }
    CMVideoDimensions size = CMVideoFormatDescriptionGetDimensions(format);
    if (size.width <= 0 || size.height <= 0 || size.width > 8192 || size.height > 8192) {
        self.error = @"映像サイズが上限を超えています。"; CFRelease(sample); return NO;
    }
    if (!session) {
        NSDictionary *spec = @{
            (__bridge NSString *)kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder: self.softwareOnly ? @NO : @YES
        };
        NSDictionary *attrs = @{
            (__bridge NSString *)kCVPixelBufferPixelFormatTypeKey: @(kCVPixelFormatType_32BGRA),
            (__bridge NSString *)kCVPixelBufferMetalCompatibilityKey: @YES,
            (__bridge NSString *)kCVPixelBufferIOSurfacePropertiesKey: @{}
        };
        VTDecompressionOutputCallbackRecord cb = { decodedFrame, (__bridge void *)self };
        OSStatus result = VTDecompressionSessionCreate(kCFAllocatorDefault, format,
            (__bridge CFDictionaryRef)spec, (__bridge CFDictionaryRef)attrs, &cb, &session);
        if(result!=noErr&&!self.softwareOnly){
            if(session){VTDecompressionSessionInvalidate(session);CFRelease(session);session=NULL;}
            self.softwareOnly=YES;
            NSDictionary *fallback=@{(__bridge NSString*)kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder:@NO};
            result=VTDecompressionSessionCreate(kCFAllocatorDefault,format,(__bridge CFDictionaryRef)fallback,(__bridge CFDictionaryRef)attrs,&cb,&session);
        }
        if (result != noErr) {
            self.error = [NSString stringWithFormat:@"VideoToolbox初期化失敗 (%d)。映像形式と復号器の対応を確認してください。", (int)result];
            CFRelease(sample); return NO;
        }
        CFTypeRef value = NULL;
        if (VTSessionCopyProperty(session, kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
                                 kCFAllocatorDefault, &value) == noErr && value) {
            atomic_store(&hardware, CFEqual(value, kCFBooleanTrue)); CFRelease(value);
        }
        OSStatus realtime = VTSessionSetProperty(session, kVTDecompressionPropertyKey_RealTime, kCFBooleanTrue);
        if (realtime != noErr) {
            self.error = [NSString stringWithFormat:@"低遅延モード設定失敗 (%d)。", (int)realtime];
            CFRelease(sample); return NO;
        }
    }
    // With both asynchronous/temporal flags clear, Apple's API completes the
    // output callback before return. This measures decode, not queue submission.
    double decodeAt=CACurrentMediaTime();
    uint64_t failuresBefore=atomic_load(&failures);
    OSStatus result = VTDecompressionSessionDecodeFrame(session, sample, 0, NULL, NULL);
    atomic_fetch_add(&decodeMicros,(uint64_t)(MAX(0.,CACurrentMediaTime()-decodeAt)*1e6));
    CFRelease(sample);
    if (result != noErr || atomic_load(&failures)>failuresBefore) {
        self.decodeFailureStreak=MIN(3,self.decodeFailureStreak+1);
        if(self.decodeFailureStreak>=3&&!self.softwareOnly&&atomic_load(&hardware)){
            VTDecompressionSessionInvalidate(session);CFRelease(session);session=NULL;
            self.softwareOnly=YES;atomic_store(&hardware,false);
        }
        self.error = [NSString stringWithFormat:@"H.264デコード失敗 (%d)。", (int)result];
        if(atomic_load(&failures)==failuresBefore)atomic_fetch_add(&failures, 1); return NO;
    }
    self.decodeFailureStreak=0;self.error=nil;
    return YES;
}
- (BOOL)feedAnnexB:(NSData *)data {
    if (data.length == 0 || data.length > 8*1024*1024) return NO;
    const uint8_t *bytes = data.bytes;
    NSMutableArray<NSData *> *nals = [NSMutableArray new];
    NSUInteger start = NSNotFound;
    for (NSUInteger i = 0; i + 3 <= data.length;) {
        NSUInteger prefix = 0;
        if (i + 4 <= data.length && bytes[i]==0 && bytes[i+1]==0 && bytes[i+2]==0 && bytes[i+3]==1) prefix=4;
        else if (bytes[i]==0 && bytes[i+1]==0 && bytes[i+2]==1) prefix=3;
        if (prefix) {
            if (start != NSNotFound && i > start) [nals addObject:[data subdataWithRange:NSMakeRange(start,i-start)]];
            start = i+prefix; i += prefix;
        } else i++;
        if (nals.count > 128) return NO;
    }
    if (start != NSNotFound && start < data.length) [nals addObject:[data subdataWithRange:NSMakeRange(start,data.length-start)]];
    if (nals.count == 0 || nals.count > 128) { self.error=@"H.264のNAL形式が不正です。"; return NO; }
    NSMutableData *avcc = [NSMutableData new]; BOOL parameterChanged = NO;
    for (NSData *nal in nals) {
        const uint8_t *p = nal.bytes; uint8_t type=p[0]&31;
        if (type==7) { if (![networkSPS isEqualToData:nal]) { networkSPS=nal; parameterChanged=YES; } }
        else if (type==8) { if (![networkPPS isEqualToData:nal]) { networkPPS=nal; parameterChanged=YES; } }
        else if (type==1 || type==5 || type==6) {
            uint32_t count=CFSwapInt32HostToBig((uint32_t)nal.length); [avcc appendBytes:&count length:4]; [avcc appendData:nal];
        }
    }
    if (!networkSPS || !networkPPS) return YES; // Wait for the next IDR/configuration.
    if (parameterChanged || !networkFormat) {
        if (session) { VTDecompressionSessionInvalidate(session); CFRelease(session); session=NULL; }
        if (networkFormat) { CFRelease(networkFormat); networkFormat=NULL; }
        const uint8_t *parameters[] = {networkSPS.bytes,networkPPS.bytes};
        size_t lengths[] = {networkSPS.length,networkPPS.length};
        OSStatus status=CMVideoFormatDescriptionCreateFromH264ParameterSets(kCFAllocatorDefault,2,parameters,lengths,4,&networkFormat);
        if (status!=noErr) {self.error=@"H.264パラメーターを読み取れません。";return NO;}
    }
    if (avcc.length==0) return YES;
    CMBlockBufferRef block=NULL;
    OSStatus status=CMBlockBufferCreateWithMemoryBlock(kCFAllocatorDefault,NULL,avcc.length,kCFAllocatorDefault,NULL,0,avcc.length,0,&block);
    if (status!=noErr) return NO;
    status=CMBlockBufferReplaceDataBytes(avcc.bytes,block,0,avcc.length);
    CMSampleBufferRef sample=NULL; size_t size=avcc.length;
    CMSampleTimingInfo timing={CMTimeMake(1,60),CMTimeMake((int64_t)atomic_load(&decoded),60),kCMTimeInvalid};
    if (status==noErr) status=CMSampleBufferCreateReady(kCFAllocatorDefault,block,networkFormat,1,1,&timing,1,&size,&sample);
    CFRelease(block);
    if (status!=noErr) return NO;
    return [self decodeSample:sample];
}
- (CVPixelBufferRef)copyLatest:(uint64_t *)frameGeneration {
    [lock lock]; CVPixelBufferRef buffer = latest ? CVPixelBufferRetain(latest) : NULL;
    *frameGeneration = atomic_load(&generation); [lock unlock];
    return buffer;
}
@end

@interface LRDSoftwareView : NSView
@property(strong) LRDDecoder *decoder;
@property(strong) CIContext *context;
@end
@implementation LRDSoftwareView
- (void)drawRect:(NSRect)dirty {
    double renderAt=CACurrentMediaTime();
    (void)dirty;
    [[NSColor blackColor] setFill]; NSRectFill(self.bounds);
    uint64_t gen = 0;
    CVPixelBufferRef buffer = [self.decoder copyLatest:&gen];
    if (!buffer) return;
    CIImage *image = [CIImage imageWithCVPixelBuffer:buffer];
    CGImageRef cg = [self.context createCGImage:image fromRect:image.extent];
    if (cg) {
        CGFloat scale = MIN(self.bounds.size.width / image.extent.size.width, self.bounds.size.height / image.extent.size.height);
        NSRect rect = NSMakeRect((self.bounds.size.width - image.extent.size.width * scale) / 2,
                                (self.bounds.size.height - image.extent.size.height * scale) / 2,
                                image.extent.size.width * scale, image.extent.size.height * scale);
        CGContextDrawImage([[NSGraphicsContext currentContext] CGContext], rect, cg);
        CGImageRelease(cg);
        if (atomic_exchange(&self.decoder->presentedGeneration, gen) != gen) {
            atomic_fetch_add(&self.decoder->presented, 1);
            atomic_fetch_add(&self.decoder->renderMicros,(uint64_t)(MAX(0.,CACurrentMediaTime()-renderAt)*1e6));
        }
    }
    CVPixelBufferRelease(buffer);
}
@end

extern bool lrd_agent_configure(uint32_t index);
#include "file-drop.inc"
@interface LRDApplication : NSObject <NSApplicationDelegate, NSWindowDelegate, MTKViewDelegate, LRDFileDropDelegate>
@property(strong) NSWindow *window;
@property(strong) NSTextField *status;
@property(strong) NSTextField *metrics;
@property(strong) NSDictionary *resourceStatistics;
@property double lastMetricsAt;
@property uint64_t lastMetricsDecoded;
@property uint64_t lastMetricsPresented;
@property uint64_t lastDecodeMicros;
@property uint64_t lastRenderMicros;
@property uint64_t lastGpuMicros;
@property uint64_t lastGpuFrames;
@property uint64_t lastMetricsErrors;
@property uint64_t lastMetricsDropped;
@property uint64_t lastMetricsReceived;
@property(strong) NSDictionary *remoteStatistics;
@property(strong) NSPanel *dashboard;
@property(strong) NSTextField *dashboardText;
@property(strong) NSView *videoContainer;
@property(strong) MTKView *metalView;
@property(strong) LRDSoftwareView *softwareView;
@property(strong) id<MTLCommandQueue> commands;
@property(strong) CIContext *context;
@property(strong) LRDDecoder *decoder;
@property(strong) NSTimer *metricsTimer;
@property(strong) NSTimer *softwareTimer;
@property(strong) dispatch_source_t decodeTimer;
@property(strong) dispatch_queue_t decodeQueue;
@property(copy) NSString *reportPath;
@property(copy) NSString *videoPath;
@property(copy) NSString *renderer;
@property(copy) NSString *lastError;
@property BOOL forceSoftware;
@property BOOL smoke;
@property int exitCode;
@property CFTimeInterval started;
@property CFTimeInterval elapsed;
@property(strong) NSPopUpButton *interfaces;
@property(strong) NSPopUpButton *hosts;
@property(strong) NSTextField *address;
@property(strong) id inputMonitor;
@property(strong) NSWindow *transferWindow;
@property(strong) NSTextField *transferStatus;
@property(strong) LRDAudio *audio;
@property BOOL audioEnabled;
@property BOOL clipboardEnabled;
@property NSInteger clipboardChange;
@property(strong) NSTimer *clipboardTimer;
@property BOOL remoteConnected;
@property BOOL captureInput;
@property BOOL awaitingFrame;
@property uint64_t lastDecodedCount;
@property double lastDecodedAt;
@property BOOL relativeMouse;
@property BOOL automaticDisplayPending;
@property double lastDecodeDurationMs;
@property double lastRenderDurationMs;
@property double lastRelativeSuggestion;
@property BOOL windowsCommand;
@property BOOL jisKeyboard;
@property(strong) NSArray *monitors;
@property NSInteger currentMonitor;
@property(strong) NSPopUpButton *displayMonitors;
@property(strong) NSPopUpButton *displayModes;
@property(strong) NSView *fullToolbar;
@property NSRect normalVideoFrame;
@property NSUInteger previousModifiers;
@property uint64_t networkGeneration;
@property uint32_t remoteWidth;
@property uint32_t remoteHeight;
@property uint32_t launchInterface;
@property(copy) NSString *launchAddress;
@property BOOL launchDiscovery;
@property double rttMilliseconds;
@property uint64_t receivedBytes;
@property uint64_t sentBytes;
- (void)networkEvent:(uint32_t)kind data:(NSData *)data;

@end

static LRDApplication *lrdApp;
static atomic_uint queuedNetworkFrames;
static atomic_uint queuedAudioEvents;
static atomic_bool requestKeyframe;
static atomic_ullong droppedNetworkFrames;

static NSTextField *label(NSString *text, CGFloat size, BOOL bold) {
    NSTextField *field = [NSTextField labelWithString:text];
    field.font = bold ? [NSFont boldSystemFontOfSize:size] : [NSFont systemFontOfSize:size];
    field.textColor = [NSColor colorWithCalibratedWhite:0.88 alpha:1];
    field.lineBreakMode = NSLineBreakByWordWrapping;
    field.maximumNumberOfLines = 0;
    return field;
}

#include "network-support.inc"

@implementation LRDApplication
#include "network-ui.inc"
#include "transfer-ui.inc"
#include "clipboard-ui.inc"
#include "display-ui.inc"
#include "agent-ui.inc"
#include "dashboard.inc"
- (void)applicationDidFinishLaunching:(NSNotification *)notification {
    (void)notification;
    lrdApp = self; atomic_init(&queuedNetworkFrames,0);atomic_init(&queuedAudioEvents,0);self.audio=[LRDAudio new];self.audioEnabled=YES;self.clipboardEnabled=YES;
    self.decodeQueue = dispatch_queue_create("local.lrd.video.decode", DISPATCH_QUEUE_SERIAL);
    self.window = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, 1120, 780)
        styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable | NSWindowStyleMaskResizable | NSWindowStyleMaskMiniaturizable
        backing:NSBackingStoreBuffered defer:NO];
    self.window.title = @"LAN Remote Desktop";
    self.window.minSize = NSMakeSize(930, 620); self.window.delegate = self;
    self.window.collectionBehavior = NSWindowCollectionBehaviorFullScreenPrimary;
    NSView *root = self.window.contentView;
    root.wantsLayer = YES; root.layer.backgroundColor = [NSColor colorWithCalibratedWhite:0.075 alpha:1].CGColor;
    NSTextField *title = label(@"LAN Remote Desktop", 26, YES);
    title.frame = NSMakeRect(28, 716, 1050, 36); title.autoresizingMask = NSViewMinYMargin | NSViewWidthSizable;
    [root addSubview:title];
    NSTextField *subtitle = label(@"Ethernet直結  /  TLS 1.3・端末ペアリング  /  H.264 · VideoToolbox · Metal", 13, NO);
    subtitle.frame = NSMakeRect(28, 682, 1050, 26); subtitle.autoresizingMask = NSViewMinYMargin | NSViewWidthSizable;
    [root addSubview:subtitle];
    self.videoContainer = [[LRDFileDropView alloc] initWithFrame:NSMakeRect(28, 168, 1064, 464)];
    [(LRDFileDropView *)self.videoContainer setFileDelegate:self];[self.videoContainer registerForDraggedTypes:@[NSPasteboardTypeFileURL]];
    self.videoContainer.wantsLayer = YES; self.videoContainer.layer.backgroundColor = [NSColor blackColor].CGColor;
    self.videoContainer.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
    [root addSubview:self.videoContainer];
    self.interfaces = [[NSPopUpButton alloc] initWithFrame:NSMakeRect(28,642,200,30) pullsDown:NO];
    [root addSubview:self.interfaces];
    self.address = [[NSTextField alloc] initWithFrame:NSMakeRect(238,646,238,24)];
    self.address.placeholderString = @"WindowsのIPv4アドレス または IP:Port";
    [root addSubview:self.address];
    NSArray *netTitles = @[@"探索", @"接続", @"切断", @"入力を再取得"];
    SEL netActions[] = {@selector(discover:),@selector(connect:),@selector(disconnect:),@selector(recapture:)};
    for (NSUInteger i=0;i<netTitles.count;i++) {
        NSButton *b=[NSButton buttonWithTitle:netTitles[i] target:self action:netActions[i]];
        b.bezelStyle=NSBezelStyleRounded; b.frame=NSMakeRect(488+i*104,640,100,32); [root addSubview:b];
    }
    for(NSView *v in root.subviews)if(v!=self.videoContainer)v.autoresizingMask|=NSViewMinYMargin;
    [self reloadInterfaces];
    NSArray *titles = @[@"映像テストを開始", @"停止", @"ファイル転送…", @"診断を保存…", @"全画面"];
    SEL actions[] = {@selector(start:), @selector(stop:), @selector(files:), @selector(export:), @selector(fullscreen:)};
    CGFloat x = 28;
    for (NSUInteger i = 0; i < titles.count; i++) {
        NSButton *button = [NSButton buttonWithTitle:titles[i] target:self action:actions[i]];
        button.bezelStyle = NSBezelStyleRounded; button.frame = NSMakeRect(x, 122, i == 2 ? 180 : 158, 32);
        [root addSubview:button]; x += i == 2 ? 192 : 170;
    }
    self.metrics = label(@"デコード —   /   描画 —   /   GPU —", 13, YES);
    self.metrics.frame = NSMakeRect(28, 82, 1064, 28); self.metrics.autoresizingMask = NSViewWidthSizable;
    [root addSubview:self.metrics];
    self.status = label(@"WindowsでHostを起動し、有線アダプターから「探索」→「接続」を選びます。", 12, NO);
    self.status.frame = NSMakeRect(28, 18, 1064, 56); self.status.autoresizingMask = NSViewWidthSizable;
    [root addSubview:self.status];
    NSMenu *menu = [NSMenu new]; NSMenuItem *appMenuItem = [NSMenuItem new]; [menu addItem:appMenuItem];
    NSMenu *appMenu = [NSMenu new];
    [appMenu addItemWithTitle:@"LAN Remote Desktopを終了" action:@selector(terminate:) keyEquivalent:@"q"];
    NSMenuItem *openVideo=[appMenu addItemWithTitle:@"検証用の映像ファイルを開く…" action:@selector(choose:) keyEquivalent:@"o"]; openVideo.target=self;
    NSMenuItem *exportLogs=[appMenu addItemWithTitle:@"性能ログを書き出す…" action:@selector(exportLogs:) keyEquivalent:@""];exportLogs.target=self;
    NSMenuItem *dashboard=[appMenu addItemWithTitle:@"性能ダッシュボード…" action:@selector(dashboard:) keyEquivalent:@""];dashboard.target=self;
    appMenuItem.submenu = appMenu; NSApp.mainMenu = menu;
    NSMenuItem *viewItem = [NSMenuItem new]; [menu addItem:viewItem];
    NSMenu *viewMenu = [[NSMenu alloc] initWithTitle:@"表示"];
    NSMenuItem *full = [viewMenu addItemWithTitle:@"全画面を切り替え" action:@selector(fullscreen:) keyEquivalent:@"f"];
    full.target = self; full.keyEquivalentModifierMask = NSEventModifierFlagControl | NSEventModifierFlagCommand;
    viewItem.submenu = viewMenu;
    NSMenuItem *mode=[viewMenu addItemWithTitle:@"相対マウスを切り替え" action:@selector(relative:) keyEquivalent:@"r"];
    mode.target=self; mode.keyEquivalentModifierMask=NSEventModifierFlagControl|NSEventModifierFlagOption;
    NSMenuItem *command=[viewMenu addItemWithTitle:@"CommandをWindowsキーに割り当て" action:@selector(commandMode:) keyEquivalent:@""];
    command.target=self;
    NSMenuItem *transportItem=[NSMenuItem new];[menu addItem:transportItem];
    NSMenu *transportMenu=[[NSMenu alloc] initWithTitle:@"通信方式"];transportItem.submenu=transportMenu;
    NSInteger transportMode=[[NSUserDefaults standardUserDefaults] integerForKey:@"transportMode"];
    if(transportMode<0||transportMode>2)transportMode=0;
    lrd_set_transport((uint8_t)transportMode);
    NSArray *transportTitles=@[@"TLS/TCP",@"TLS＋暗号化UDP",@"QUIC"];
    for(NSUInteger i=0;i<transportTitles.count;i++){
        NSMenuItem *item=[transportMenu addItemWithTitle:transportTitles[i] action:@selector(transportMode:) keyEquivalent:@""];
        item.target=self;item.tag=(NSInteger)i;item.state=(i==(NSUInteger)transportMode)?NSControlStateValueOn:NSControlStateValueOff;
    }
    NSEventMask mask=NSEventMaskKeyDown|NSEventMaskKeyUp|NSEventMaskFlagsChanged|NSEventMaskMouseMoved|NSEventMaskLeftMouseDown|NSEventMaskLeftMouseUp|NSEventMaskRightMouseDown|NSEventMaskRightMouseUp|NSEventMaskOtherMouseDown|NSEventMaskOtherMouseUp|NSEventMaskLeftMouseDragged|NSEventMaskRightMouseDragged|NSEventMaskOtherMouseDragged|NSEventMaskScrollWheel;
    __weak LRDApplication *weakSelf=self;
    self.inputMonitor=[NSEvent addLocalMonitorForEventsMatchingMask:mask handler:^NSEvent *(NSEvent *e){return [weakSelf handleInput:e];}];
    NSMenuItem *clipboard=[viewMenu addItemWithTitle:@"クリップボード同期" action:@selector(clipboardMode:) keyEquivalent:@""];clipboard.target=self;clipboard.state=NSControlStateValueOn;
    NSArray *audioTitles=@[@"音声: Windowsのみ",@"音声: 両方から再生",@"音声: Macのみ"];
    for(NSUInteger i=0;i<audioTitles.count;i++){NSMenuItem *item=[viewMenu addItemWithTitle:audioTitles[i] action:@selector(audioMode:) keyEquivalent:@""];item.target=self;item.tag=(NSInteger)i;}
    NSMenuItem *display=[viewMenu addItemWithTitle:@"モニターと表示モード…" action:@selector(displaySettings:) keyEquivalent:@"m"];display.target=self;display.keyEquivalentModifierMask=NSEventModifierFlagControl|NSEventModifierFlagOption;
    NSMenuItem *automatic=[viewMenu addItemWithTitle:@"Macに合わせてWindows解像度を自動調整" action:@selector(automaticDisplay:) keyEquivalent:@""];automatic.target=self;
    automatic.state=[[NSUserDefaults standardUserDefaults] boolForKey:@"keepWindowsResolution"]?NSControlStateValueOff:NSControlStateValueOn;
    self.jisKeyboard=KBGetLayoutType(LMGetKbdType())==kKeyboardJIS;
    NSMenuItem *agent=[appMenu addItemWithTitle:@"ログイン時に選択Ethernetを監視" action:@selector(agent:) keyEquivalent:@""];agent.target=self;
    agent.state=[[NSFileManager defaultManager] fileExistsAtPath:[NSHomeDirectory() stringByAppendingPathComponent:@"Library/LaunchAgents/local.lanremotedesktop.agent.plist"]]?NSControlStateValueOn:NSControlStateValueOff;
    [[NSDistributedNotificationCenter defaultCenter] addObserver:self selector:@selector(agentNotification:) name:@"local.lanremotedesktop.discovered" object:nil suspensionBehavior:NSNotificationSuspensionBehaviorDeliverImmediately];
    self.window.acceptsMouseMovedEvents=YES;
    NSRect available=NSScreen.mainScreen.visibleFrame;[self.window setContentSize:NSMakeSize(MIN(1120,MAX(930,available.size.width-40)),MIN(780,MAX(600,available.size.height-40)))];
    [self.window center]; [self.window makeKeyAndOrderFront:nil]; [NSApp activateIgnoringOtherApps:YES];
    if(!self.smoke)lrd_startup_ready();
    if (self.smoke) [self start:nil];
    if(self.launchAddress){[self.interfaces selectItemWithTag:self.launchInterface];self.address.stringValue=self.launchAddress;[self connect:nil];}
    else if(self.launchDiscovery){[self.interfaces selectItemWithTag:self.launchInterface];[self discover:nil];}
}
- (void)fullscreen:(id)sender { (void)sender; [self.window toggleFullScreen:nil]; }
- (void)choose:(id)sender {
    (void)sender;
    NSOpenPanel *panel = [NSOpenPanel openPanel]; panel.allowedFileTypes = @[@"mp4", @"mov", @"m4v"];
    panel.canChooseDirectories = NO; panel.allowsMultipleSelection = NO;
    if ([panel runModal] == NSModalResponseOK) { self.videoPath = panel.URL.path; [self start:nil]; }
}
- (void)start:(id)sender {
    (void)sender; if (self.remoteConnected) [self disconnect:nil]; [self stop:nil]; self.lastError = nil; self.exitCode = 0;
    NSString *path = self.videoPath ?: [[NSBundle mainBundle] pathForResource:@"test-pattern" ofType:@"mp4"];
    if (!path) { self.status.stringValue = @"検証映像がありません。「映像ファイルを選択」からH.264のMP4を選んでください。"; self.lastError = self.status.stringValue; self.exitCode = 2; if (self.smoke) { [self writeReport:self.reportPath]; [NSApp terminate:nil]; } return; }
    self.decoder = [LRDDecoder new]; self.decoder.softwareOnly = self.forceSoftware;
    self.started = CACurrentMediaTime(); self.elapsed = 0;
    [self prepareRenderer];
    self.status.stringValue = @"ローカル検証中。実測値はこのMacの結果です。High Sierraの互換性は別途確認が必要です。";
    LRDDecoder *decoder = self.decoder;
    self.decodeTimer = dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER, 0, 0, self.decodeQueue);
    dispatch_source_set_timer(self.decodeTimer, dispatch_time(DISPATCH_TIME_NOW, 0), NSEC_PER_SEC/60, NSEC_PER_MSEC);
    __block BOOL opened = NO;
    __block BOOL finished = NO;
    dispatch_source_set_event_handler(self.decodeTimer, ^{
        @autoreleasepool {
            if (finished) return;
            BOOL ok = opened || [decoder open:[NSURL fileURLWithPath:path]]; opened = YES;
            if (ok) ok = [decoder step];
            if (!ok) {
                finished = YES;
                if (decoder->session) VTDecompressionSessionWaitForAsynchronousFrames(decoder->session);
                dispatch_async(dispatch_get_main_queue(), ^{
                    if (self.decoder != decoder || !self.decodeTimer) return;
                    self.lastError = decoder.error;
                    if (decoder.error) { self.exitCode = 3; self.status.stringValue = decoder.error; }
                    else self.status.stringValue = @"検証映像の再生が完了しました。診断を保存して実機結果を記録できます。";
                    [self stop:nil];
                    if (self.smoke) { if (atomic_load(&decoder->presented) == 0 && self.exitCode == 0) self.exitCode = 4; [self writeReport:self.reportPath]; [NSApp terminate:nil]; }
                });
            }
        }
    });
    dispatch_resume(self.decodeTimer);
    self.metricsTimer = [NSTimer scheduledTimerWithTimeInterval:0.5 repeats:YES block:^(NSTimer *timer) { (void)timer; [self updateMetrics]; }];
    if (self.smoke) {
        dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 15 * NSEC_PER_SEC), dispatch_get_main_queue(), ^{
            self.lastError = @"検証タイムアウト"; self.exitCode = 5; [self stop:nil]; [self writeReport:self.reportPath]; [NSApp terminate:nil];
        });
    }
}
- (void)prepareRenderer {
    for (NSView *view in self.videoContainer.subviews.copy) [view removeFromSuperview];
    id<MTLDevice> device = self.forceSoftware ? nil : MTLCreateSystemDefaultDevice();
    if (device) {
        self.metalView = [[MTKView alloc] initWithFrame:self.videoContainer.bounds device:device];
        self.metalView.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
        self.metalView.framebufferOnly = NO; self.metalView.preferredFramesPerSecond = 60;
        self.metalView.clearColor = MTLClearColorMake(0, 0, 0, 1); self.metalView.delegate = self;
        self.commands = [device newCommandQueue]; self.context = [CIContext contextWithMTLDevice:device];
        if(!self.commands||!self.context){self.forceSoftware=YES;[self prepareRenderer];return;}
        self.renderer = @"Metal"; [self.videoContainer addSubview:self.metalView];
    } else {
        self.softwareView = [[LRDSoftwareView alloc] initWithFrame:self.videoContainer.bounds];
        self.softwareView.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
        self.softwareView.decoder = self.decoder;
        self.softwareView.context = [CIContext contextWithOptions:@{ kCIContextUseSoftwareRenderer: @YES }];
        self.renderer = @"Core Image CPU fallback"; [self.videoContainer addSubview:self.softwareView];
        self.softwareTimer = [NSTimer scheduledTimerWithTimeInterval:1.0/60 repeats:YES block:^(NSTimer *timer) { (void)timer; self.softwareView.needsDisplay = YES; }];
    }
}
- (void)stop:(id)sender {
    self.lastMetricsAt=0;self.lastMetricsDecoded=0;self.lastMetricsPresented=0;
    if (sender && self.remoteConnected) { [self disconnect:nil]; return; }
    (void)sender;
    if (self.decodeTimer) { dispatch_source_cancel(self.decodeTimer); self.decodeTimer = nil; dispatch_sync(self.decodeQueue, ^{}); }
    [self.metricsTimer invalidate]; self.metricsTimer = nil;
    [self.clipboardTimer invalidate];self.clipboardTimer=nil;[self.audio stop];
    [self.softwareTimer invalidate]; self.softwareTimer = nil;
    if (self.decoder && self.elapsed == 0) self.elapsed = CACurrentMediaTime() - self.started;
    [self updateMetrics]; self.metalView.paused = YES;
    if (sender) self.status.stringValue = @"映像テストを停止しました。";
}
- (void)updateMetrics {
    if (!self.decoder) return;
    double seconds = MAX(0.001, self.elapsed > 0 ? self.elapsed : CACurrentMediaTime() - self.started);
    uint64_t decodedCount=atomic_load(&self.decoder->decoded);double now=CACurrentMediaTime();
    uint64_t shown=atomic_load(&self.decoder->presented);
    if(shown!=self.lastDecodedCount){self.lastDecodedCount=shown;self.lastDecodedAt=now;
        if(self.remoteConnected && self.awaitingFrame && self.window.isKeyWindow){self.awaitingFrame=NO;self.captureInput=YES;self.status.stringValue=@"映像を受信しました。⌃⌥⌘Escで入力をMacへ戻せます。";}
    }
    if(self.remoteConnected && self.captureInput && now-self.lastDecodedAt>3){[self sendInput:@{@"event":@"Release"}];[self releaseRelativeMouse];self.captureInput=NO;self.awaitingFrame=YES;self.status.stringValue=@"映像の更新を待っています。入力を一時停止しました。";}
    if(self.remoteConnected && self.captureInput)[self sendInput:@{@"event":@"KeepAlive"}];
    self.metrics.stringValue = [NSString stringWithFormat:@"デコード %.1f fps  /  描画 %.1f fps  /  %@  /  H.264 %@  /  エラー %llu",
        atomic_load(&self.decoder->decoded)/seconds, atomic_load(&self.decoder->presented)/seconds,
        self.renderer, atomic_load(&self.decoder->hardware) ? @"GPU" : @"software / 未初期化",
        (unsigned long long)atomic_load(&self.decoder->failures)];
    if(self.remoteConnected)self.metrics.stringValue=[self.metrics.stringValue stringByAppendingFormat:@"  /  RTT %.1f ms  /  %.1f MiB",self.rttMilliseconds,self.receivedBytes/1048576.0];
    char *raw=lrd_resource_statistics_json();
    if(raw){NSString *json=[NSString stringWithUTF8String:raw];lrd_free_string(raw);NSData *data=[json dataUsingEncoding:NSUTF8StringEncoding];
        id value=data?[NSJSONSerialization JSONObjectWithData:data options:0 error:NULL]:nil;
        if([value isKindOfClass:[NSDictionary class]])self.resourceStatistics=value;}
    NSNumber *cpu=self.resourceStatistics[@"cpu_percent"],*memory=self.resourceStatistics[@"resident_bytes"];
    if([cpu isKindOfClass:[NSNumber class]] && [memory isKindOfClass:[NSNumber class]])self.metrics.stringValue=[self.metrics.stringValue stringByAppendingFormat:@"  /  CPU %.1f%%  /  RAM %.1f MiB",cpu.doubleValue,memory.doubleValue/1048576.0];
    uint64_t presentedCount=atomic_load(&self.decoder->presented);double dt=MAX(0.001,now-self.lastMetricsAt);
    uint64_t decodeUs=atomic_load(&self.decoder->decodeMicros),renderUs=atomic_load(&self.decoder->renderMicros),gpuUs=atomic_load(&self.decoder->gpuMicros),gpuFrames=atomic_load(&self.decoder->gpuFrames);
    uint64_t errors=atomic_load(&self.decoder->failures),dropped=atomic_load(&droppedNetworkFrames);
    if(self.remoteConnected && self.lastMetricsAt>0 && decodedCount>=self.lastMetricsDecoded && presentedCount>=self.lastMetricsPresented){
        uint64_t decoded=decodedCount-self.lastMetricsDecoded,presented=presentedCount-self.lastMetricsPresented,gpu=gpuFrames-self.lastGpuFrames;
        if(decoded>0)self.lastDecodeDurationMs=(decodeUs-self.lastDecodeMicros)/(1000.*decoded);
        if(presented>0)self.lastRenderDurationMs=(renderUs-self.lastRenderMicros)/(1000.*presented);
        NSDictionary *window=@{@"interval_ms":@((uint32_t)MIN(10001,dt*1000)),@"decoded":@(decoded),@"presented":@(presented),
            @"decode_ms":@(decoded?(decodeUs-self.lastDecodeMicros)/(1000.*decoded):0.),
            @"render_ms":@(presented?(renderUs-self.lastRenderMicros)/(1000.*presented):0.),
            @"render_gpu_ms":gpu?@((gpuUs-self.lastGpuMicros)/(1000.*gpu)):(id)NSNull.null,
            @"errors":@(errors),@"dropped":@(dropped),@"errors_delta":@(errors-self.lastMetricsErrors),@"dropped_delta":@(dropped-self.lastMetricsDropped),
            @"received_delta":@(self.receivedBytes>=self.lastMetricsReceived?self.receivedBytes-self.lastMetricsReceived:0),
            @"sent":@(self.sentBytes),@"received":@(self.receivedBytes),@"hardware":@(atomic_load(&self.decoder->hardware)),
            @"visible":@(self.window.isVisible && !self.window.isMiniaturized && (self.window.occlusionState&NSWindowOcclusionStateVisible)!=0),@"rtt_ms":@(self.rttMilliseconds)};
        NSData *json=[NSJSONSerialization dataWithJSONObject:window options:0 error:NULL];if(json)lrd_record_metrics_json(json.bytes,json.length);
    }
    self.lastMetricsAt=now;self.lastMetricsDecoded=decodedCount;self.lastMetricsPresented=presentedCount;
    self.lastDecodeMicros=decodeUs;self.lastRenderMicros=renderUs;self.lastGpuMicros=gpuUs;self.lastGpuFrames=gpuFrames;
    self.lastMetricsErrors=errors;self.lastMetricsDropped=dropped;self.lastMetricsReceived=self.receivedBytes;
    [self updateDashboard];
    [self applyAutomaticDisplay];
}
- (void)exportLogs:(id)sender {
    (void)sender;NSSavePanel *panel=[NSSavePanel savePanel];panel.allowedFileTypes=@[@"json"];panel.nameFieldStringValue=@"LAN-Remote-Desktop-diagnostics.json";
    if([panel runModal]!=NSModalResponseOK)return;NSString *path=panel.URL.path;
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_UTILITY,0),^{BOOL ok=lrd_export_numeric_logs(path.UTF8String);
        dispatch_async(dispatch_get_main_queue(),^{self.status.stringValue=ok?@"性能の数値ログを書き出しました。画面・入力・クリップボード本文は含みません。":@"ログを書き出せませんでした。アプリの保存領域以外の保存先を選択してください。";});});
}
- (void)drawInMTKView:(MTKView *)view {
    double renderAt=CACurrentMediaTime();
    LRDDecoder *decoder = self.decoder;
    uint64_t gen = 0;
    CVPixelBufferRef buffer = [decoder copyLatest:&gen];
    if (!buffer) return;
    id<CAMetalDrawable> drawable = view.currentDrawable;
    id<MTLCommandBuffer> command = [self.commands commandBuffer];
    if (!drawable || !command) { CVPixelBufferRelease(buffer); return; }
    CIImage *image = [CIImage imageWithCVPixelBuffer:buffer];
    CGFloat width = drawable.texture.width, height = drawable.texture.height;
    CGFloat scale = MIN(width/image.extent.size.width, height/image.extent.size.height);
    image = [image imageByApplyingTransform:CGAffineTransformMakeScale(scale, scale)];
    image = [image imageByApplyingTransform:CGAffineTransformMakeTranslation((width-image.extent.size.width)/2, (height-image.extent.size.height)/2)];
    CIImage *background = [[CIImage imageWithColor:[CIColor colorWithRed:0 green:0 blue:0 alpha:1]] imageByCroppingToRect:CGRectMake(0,0,width,height)];
    image = [image imageByCompositingOverImage:background];
    CGColorSpaceRef colors = CGColorSpaceCreateDeviceRGB();
    [self.context render:image toMTLTexture:drawable.texture commandBuffer:command bounds:CGRectMake(0,0,width,height) colorSpace:colors];
    CGColorSpaceRelease(colors);
    [command addCompletedHandler:^(id<MTLCommandBuffer> completed) {
        if (completed.status == MTLCommandBufferStatusCompleted) {
            uint64_t previous = atomic_load(&decoder->presentedGeneration);
            if (gen > previous && atomic_compare_exchange_strong(&decoder->presentedGeneration, &previous, gen)) {
                atomic_fetch_add(&decoder->presented, 1);
                atomic_fetch_add(&decoder->renderMicros,(uint64_t)(MAX(0.,CACurrentMediaTime()-renderAt)*1e6));
                if(@available(macOS 10.15,*)) {
                    double gpu=completed.GPUEndTime-completed.GPUStartTime;
                    if(isfinite(gpu)&&gpu>=0&&completed.GPUStartTime>0){atomic_fetch_add(&decoder->gpuMicros,(uint64_t)(gpu*1e6));atomic_fetch_add(&decoder->gpuFrames,1);}
                }
            }
        } else {
            atomic_fetch_add(&decoder->failures, 1);
            dispatch_async(dispatch_get_main_queue(),^{
                if(self.decoder!=decoder||self.forceSoftware)return;
                if(self.remoteConnected)[self sendInput:@{@"event":@"Release"}];
                [self releaseRelativeMouse];self.captureInput=NO;self.awaitingFrame=self.remoteConnected;
                self.forceSoftware=YES;[self prepareRenderer];
                self.status.stringValue=@"GPU描画を継続できないためCPU描画へ切り替えました。表示の復帰を待っています。";
            });
        }
    }];
    [command presentDrawable:drawable]; [command commit]; CVPixelBufferRelease(buffer);
}
- (void)mtkView:(MTKView *)view drawableSizeWillChange:(CGSize)size { (void)view; (void)size; }
- (NSDictionary *)report {
    double seconds = MAX(0.001, self.elapsed > 0 ? self.elapsed : CACurrentMediaTime() - self.started);
    uint64_t count = self.decoder ? atomic_load(&self.decoder->decoded) : 0;
    uint64_t rendered = self.decoder ? atomic_load(&self.decoder->presented) : 0;
    return @{@"schema": @1, @"test": self.remoteConnected ? @"remote_h264_decode_render" : @"local_h264_decode_render", @"os": NSProcessInfo.processInfo.operatingSystemVersionString,
        @"architecture": @LRD_ARCH, @"renderer": self.renderer ?: @"none", @"local_statistics":self.resourceStatistics?:@{},@"remote_statistics":self.remoteStatistics?:@{},
        @"hardware_decode": (self.decoder && atomic_load(&self.decoder->hardware) ? @YES : @NO),
        @"decoded_frames": @(count), @"rendered_frames": @(rendered), @"elapsed_seconds": @(seconds),
        @"decode_fps": @(count/seconds), @"render_fps": @(rendered/seconds),
        @"decode_or_gpu_errors": @(self.decoder ? atomic_load(&self.decoder->failures) : 0),
        @"received_bytes":@(self.receivedBytes),@"sent_bytes":@(self.sentBytes),@"rtt_ms":@(self.rttMilliseconds),@"dropped_network_frames":@(atomic_load(&droppedNetworkFrames)),@"remote_connection_tested":self.remoteConnected ? @YES : @NO, @"error": self.lastError ?: @""};
}
- (void)writeReport:(NSString *)path {
    if (!path) return;
    NSError *error = nil;
    NSData *data = [NSJSONSerialization dataWithJSONObject:[self report] options:NSJSONWritingPrettyPrinted error:&error];
    if (!data || ![data writeToFile:path options:NSDataWritingAtomic error:&error]) {
        self.status.stringValue = error.localizedDescription ?: @"診断の保存に失敗しました。"; self.exitCode = 6;
    }
}
- (void)export:(id)sender {
    (void)sender; NSSavePanel *panel = [NSSavePanel savePanel]; panel.nameFieldStringValue = @"lrd-video-diagnostics.json";
    panel.allowedFileTypes = @[@"json"];
    if ([panel runModal] == NSModalResponseOK) [self writeReport:panel.URL.path];
}
- (BOOL)applicationShouldTerminateAfterLastWindowClosed:(NSApplication *)app { (void)app; return YES; }
- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)app {
    (void)app; [self disconnect:nil]; [self stop:nil];
    if (self.smoke) exit(self.exitCode);
    return NSTerminateNow;
}
@end

void lrd_native_network_event(uint64_t generation,uint32_t kind,const uint8_t *bytes,size_t length) {
    if (length>8*1024*1024 || (!bytes && length)) return;
    if(kind==3 && atomic_fetch_add(&queuedNetworkFrames,1)>=4){atomic_fetch_sub(&queuedNetworkFrames,1);atomic_fetch_add(&droppedNetworkFrames,1);atomic_store(&requestKeyframe,true);return;}
    if(kind==8 && atomic_fetch_add(&queuedAudioEvents,1)>=8){atomic_fetch_sub(&queuedAudioEvents,1);return;}
    @autoreleasepool {
        NSData *data=[NSData dataWithBytes:bytes length:length];
        dispatch_async(dispatch_get_main_queue(), ^{
            if (!lrdApp || generation<lrdApp.networkGeneration) {
                if(kind==3)atomic_fetch_sub(&queuedNetworkFrames,1);
                if(kind==8)atomic_fetch_sub(&queuedAudioEvents,1);
                return;
            }
            lrdApp.networkGeneration=generation;
            if (kind==3) {
                if (!lrdApp.remoteConnected){atomic_fetch_sub(&queuedNetworkFrames,1);return;}
                if(atomic_exchange(&requestKeyframe,false))[lrdApp sendControl:@{@"keyframe":@YES}];
                LRDDecoder *decoder=lrdApp.decoder;
                dispatch_async(lrdApp.decodeQueue, ^{
                    @autoreleasepool {
                        BOOL valid=[decoder feedAnnexB:data];atomic_fetch_sub(&queuedNetworkFrames,1);
                        if(!valid)dispatch_async(dispatch_get_main_queue(), ^{
                            [lrdApp sendControl:@{@"keyframe":@YES}];
                            if(lrdApp.decoder!=decoder)return;
                            [lrdApp sendInput:@{@"event":@"Release"}];[lrdApp releaseRelativeMouse];lrdApp.captureInput=NO;lrdApp.awaitingFrame=YES;
                            lrdApp.status.stringValue=decoder.error?:@"映像の復号に失敗しました。入力を解放しました。";
                        });
                    }
                });
            } else {
                [lrdApp networkEvent:kind data:data];
                if(kind==8)atomic_fetch_sub(&queuedAudioEvents,1);
            }
        });
    }
}

bool lrd_native_approval(const char *fingerprint,const char *code,bool first) {
    NSString *peer=[NSString stringWithUTF8String:fingerprint];NSString *number=[NSString stringWithUTF8String:code];
    __block BOOL approved=NO;
    dispatch_sync(dispatch_get_main_queue(), ^{
        NSAlert *alert=[NSAlert new];
        alert.messageText=first ? @"初回ペアリングの確認" : @"Windowsへ接続しますか？";
        alert.informativeText=first ? [NSString stringWithFormat:@"Windows側にも同じコードが表示されていることを確認してください。\n\n%@\n\n端末: %@",number,peer] : [NSString stringWithFormat:@"登録済みWindowsに接続し、画面と入力を共有します。\n\n端末: %@",peer];
        [alert addButtonWithTitle:first ? @"一致を確認して接続" : @"接続"];[alert addButtonWithTitle:@"キャンセル"];
        approved=[alert runModal]==NSAlertFirstButtonReturn;
    });
    return approved;
}

int lrd_client_main(int argc, const char *const *argv) {
    @autoreleasepool {
        LRDApplication *delegate = [LRDApplication new];
        for (int i = 1; i < argc; i++) {
            NSString *arg = [NSString stringWithUTF8String:argv[i]];
            if ([arg isEqualToString:@"--help"]) {
                puts("LAN Remote Desktop macOS client\n  --video <H.264.mp4>\n  --smoke-test --report <result.json>\n  --software (explicit CPU renderer/decoder fallback)\nConnect to the Windows Host from the Ethernet toolbar."); return 0;
            } else if ([arg isEqualToString:@"--connect"] && i+2<argc){delegate.launchInterface=(uint32_t)strtoul(argv[++i],NULL,10);delegate.launchAddress=[NSString stringWithUTF8String:argv[++i]];}
            else if ([arg isEqualToString:@"--discover"] && i+1<argc){delegate.launchInterface=(uint32_t)strtoul(argv[++i],NULL,10);delegate.launchDiscovery=YES;}
            else if ([arg isEqualToString:@"--smoke-test"]) delegate.smoke = YES;
            else if ([arg isEqualToString:@"--software"]) delegate.forceSoftware = YES;
            else if (([arg isEqualToString:@"--video"] || [arg isEqualToString:@"--report"]) && i + 1 < argc) {
                NSString *value = [NSString stringWithUTF8String:argv[++i]];
                if ([arg isEqualToString:@"--video"]) delegate.videoPath = value; else delegate.reportPath = value;
            } else { fprintf(stderr, "Unknown or incomplete argument: %s\n", argv[i]); return 1; }
        }
        NSApplication *app = [NSApplication sharedApplication];
        [app setActivationPolicy:NSApplicationActivationPolicyRegular]; app.delegate = delegate;
        [app run]; return delegate.exitCode;
    }
}

// Offline fixture hook used by the Rust TLS→NAL→VideoToolbox integration test.
uint64_t lrd_decode_annexb_probe(const uint8_t *bytes,size_t length) {
    @autoreleasepool {
        if (!bytes || length>8*1024*1024) return 0;
        LRDDecoder *decoder=[LRDDecoder new];decoder.softwareOnly=YES;
        if (![decoder feedAnnexB:[NSData dataWithBytes:bytes length:length]]) return 0;
        if (decoder->session) VTDecompressionSessionWaitForAsynchronousFrames(decoder->session);
        return atomic_load(&decoder->hardware)?0:atomic_load(&decoder->decoded);
    }
}

void lrd_agent_discovered(uint32_t index,const char *address){
    @autoreleasepool{
        NSString *host=[NSString stringWithUTF8String:address];
        NSArray *apps=[NSRunningApplication runningApplicationsWithBundleIdentifier:@"local.lanremotedesktop.client"];
        BOOL running=NO;for(NSRunningApplication *app in apps)if(app.processIdentifier!=getpid())running=YES;
        if(running)[[NSDistributedNotificationCenter defaultCenter] postNotificationName:@"local.lanremotedesktop.discovered" object:nil userInfo:@{@"index":@(index),@"address":host} deliverImmediately:YES];
        else{
            NSURL *url=[NSBundle mainBundle].bundleURL;
            [[NSWorkspace sharedWorkspace] launchApplicationAtURL:url options:NSWorkspaceLaunchDefault configuration:@{NSWorkspaceLaunchConfigurationArguments:@[@"--discover",[@(index) stringValue]]} error:NULL];
        }
    }
}

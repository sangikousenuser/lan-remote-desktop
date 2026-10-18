- (void)files:(id)sender {
    (void)sender;
    if (!self.transferWindow) {
        self.transferWindow=[[NSWindow alloc] initWithContentRect:NSMakeRect(0,0,650,300) styleMask:NSWindowStyleMaskTitled|NSWindowStyleMaskClosable backing:NSBackingStoreBuffered defer:NO];
        LRDFileDropView *drop=[[LRDFileDropView alloc] initWithFrame:self.transferWindow.contentView.bounds];drop.fileDelegate=self;[drop registerForDraggedTypes:@[NSPasteboardTypeFileURL]];self.transferWindow.contentView=drop;
        self.transferWindow.title=@"ファイル転送";self.transferWindow.releasedWhenClosed=NO;
        NSTextField *heading=[NSTextField labelWithString:@"Windows ↔ Mac ファイル転送"];
        heading.font=[NSFont boldSystemFontOfSize:20];heading.frame=NSMakeRect(24,242,600,32);[self.transferWindow.contentView addSubview:heading];
        NSTextField *help=[NSTextField wrappingLabelWithString:@"接続中の端末へファイルやフォルダーを送信します。受信先は各端末の Downloads / LAN Remote Desktop です。中断した転送は、同じ端末への再接続時に検証済みチャンクから再開します。"];
        help.frame=NSMakeRect(24,155,600,78);[self.transferWindow.contentView addSubview:help];
        NSArray *titles=@[@"ファイル/フォルダーを送信…",@"受信先を開く",@"転送をキャンセル"];
        SEL actions[]={@selector(sendFiles:),@selector(openDownloads:),@selector(cancelFiles:)};
        for (NSUInteger i=0;i<3;i++){NSButton *b=[NSButton buttonWithTitle:titles[i] target:self action:actions[i]];b.bezelStyle=NSBezelStyleRounded;b.frame=NSMakeRect(24+i*204,108,198,34);[self.transferWindow.contentView addSubview:b];}
        self.transferStatus=[NSTextField wrappingLabelWithString:@"この画面へファイル/フォルダーをドロップしても送れます。Windows側は通知領域アイコンのメニューから送信できます。"];
        self.transferStatus.frame=NSMakeRect(24,24,600,72);[self.transferWindow.contentView addSubview:self.transferStatus];
        [self.transferWindow center];
    }
    [self.transferWindow makeKeyAndOrderFront:nil];
}
- (void)sendFiles:(id)sender {
    (void)sender;
    if (!self.remoteConnected){self.transferStatus.stringValue=@"Windowsに接続してから送信してください。";return;}
    NSOpenPanel *panel=[NSOpenPanel openPanel];panel.canChooseDirectories=YES;panel.canChooseFiles=YES;panel.allowsMultipleSelection=YES;
    if ([panel runModal]!=NSModalResponseOK)return;
    if (panel.URLs.count>4){self.transferStatus.stringValue=@"同時に選択できるファイル/フォルダーは4件までです。";return;}
    for (NSURL *url in panel.URLs) lrd_send_file(url.path.UTF8String);
    self.transferStatus.stringValue=@"転送元のハッシュを計算しています…";
}
- (void)openDownloads:(id)sender {
    (void)sender;NSString *path=[NSHomeDirectory() stringByAppendingPathComponent:@"Downloads/LAN Remote Desktop"];
    NSError *error=nil;
    if (![[NSFileManager defaultManager] createDirectoryAtPath:path withIntermediateDirectories:YES attributes:nil error:&error]) {self.transferStatus.stringValue=error.localizedDescription;return;}
    [[NSWorkspace sharedWorkspace] openURL:[NSURL fileURLWithPath:path]];
}
- (void)cancelFiles:(id)sender {(void)sender;lrd_cancel_files();self.transferStatus.stringValue=@"転送をキャンセルしています…";}

- (BOOL)acceptFileURLs:(NSArray<NSURL *> *)urls {
    [self files:nil];
    if(!self.remoteConnected){self.transferStatus.stringValue=@"Windowsに接続してからドロップしてください。";return NO;}
    if(urls.count>4)return NO;
    for(NSURL *url in urls){if(!url.isFileURL)return NO;}
    for(NSURL *url in urls)lrd_send_file(url.path.UTF8String);
    self.transferStatus.stringValue=@"受信先Downloads / LAN Remote Desktopへ送信します。転送元を検証中…";return YES;
}

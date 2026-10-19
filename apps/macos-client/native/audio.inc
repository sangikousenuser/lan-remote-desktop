#import <AudioToolbox/AudioToolbox.h>
@interface LRDAudio : NSObject {
@public AudioQueueRef queue; atomic_uint buffers; uint32_t rate; uint8_t channels;
}
- (BOOL)play:(NSData *)data;
- (void)stop;
@end
static void audioFinished(void *context,AudioQueueRef queue,AudioQueueBufferRef buffer) {
    LRDAudio *audio=(__bridge LRDAudio *)context;
    AudioQueueFreeBuffer(queue,buffer);atomic_fetch_sub(&audio->buffers,1);
}
@implementation LRDAudio
- (instancetype)init {self=[super init];if(self)atomic_init(&buffers,0);return self;}
- (void)dealloc {[self stop];}
- (void)stop {if(queue){AudioQueueStop(queue,true);AudioQueueDispose(queue,true);queue=NULL;}atomic_store(&buffers,0);}
- (BOOL)play:(NSData *)data {
    if(data.length<13 || data.length>65536)return NO;
    const uint8_t *b=data.bytes;uint32_t newRate=0;memcpy(&newRate,b,4);newRate=CFSwapInt32BigToHost(newRate);uint8_t newChannels=b[4];
    if(newRate<8000 || newRate>192000 || newChannels<1 || newChannels>2 || (data.length-13)%(newChannels*4)!=0)return NO;
    if(queue && (rate!=newRate || channels!=newChannels))[self stop];
    BOOL fresh=!queue;
    if(fresh){rate=newRate;channels=newChannels;
        AudioStreamBasicDescription format={0};format.mSampleRate=rate;format.mFormatID=kAudioFormatLinearPCM;format.mFormatFlags=kAudioFormatFlagIsFloat|kAudioFormatFlagIsPacked;format.mBytesPerPacket=channels*4;format.mFramesPerPacket=1;format.mBytesPerFrame=channels*4;format.mChannelsPerFrame=channels;format.mBitsPerChannel=32;
        if(AudioQueueNewOutput(&format,audioFinished,(__bridge void *)self,NULL,NULL,0,&queue)!=noErr)return NO;
    }
    if(data.length==13 || atomic_load(&buffers)>=6)return YES;
    AudioQueueBufferRef buffer=NULL;
    if(AudioQueueAllocateBuffer(queue,(UInt32)data.length-13,&buffer)!=noErr){[self stop];return NO;}
    memcpy(buffer->mAudioData,b+13,data.length-13);buffer->mAudioDataByteSize=(UInt32)data.length-13;
    // Keep malformed floating point values away from the audio device.
    float *samples=buffer->mAudioData;for(NSUInteger i=0;i<(data.length-13)/4;i++)samples[i]=isfinite(samples[i])?fmaxf(-1,fminf(1,samples[i])):0;
    atomic_fetch_add(&buffers,1);
    if(AudioQueueEnqueueBuffer(queue,buffer,0,NULL)!=noErr){atomic_fetch_sub(&buffers,1);AudioQueueFreeBuffer(queue,buffer);[self stop];return NO;}
    if(fresh && AudioQueueStart(queue,NULL)!=noErr){[self stop];return NO;}
    return YES;
}
@end

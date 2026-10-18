- (void)clipboardMode:(NSMenuItem *)sender {
    self.clipboardEnabled=!self.clipboardEnabled;sender.state=self.clipboardEnabled?NSControlStateValueOn:NSControlStateValueOff;
    [self sendControl:@{@"clipboard":self.clipboardEnabled?@YES:@NO}];
}
- (void)audioMode:(NSMenuItem *)sender {
    if(sender.tag==2){NSAlert *alert=[NSAlert new];alert.messageText=@"Windows側だけの出音停止は現在利用できません";alert.informativeText=@"「両方から再生」を利用できます。Macだけで聴く場合は、Windowsにつないだスピーカー本体の音量を下げてください。";[alert runModal];return;}
    self.audioEnabled=sender.tag==1;if(!self.audioEnabled)[self.audio stop];
    [self sendControl:@{@"audio":self.audioEnabled?@1:@0}];
}
- (void)sendControl:(NSDictionary *)control {NSData *data=[NSJSONSerialization dataWithJSONObject:control options:0 error:NULL];if(data)lrd_control_json(data.bytes,data.length);}
- (void)pollClipboard {
    if(!self.remoteConnected || !self.clipboardEnabled)return;
    NSPasteboard *pasteboard=NSPasteboard.generalPasteboard;
    if(pasteboard.changeCount==self.clipboardChange)return;
    NSData *image=[pasteboard dataForType:NSPasteboardTypePNG];
    if(!image){NSData *tiff=[pasteboard dataForType:NSPasteboardTypeTIFF];if(tiff){NSBitmapImageRep *bitmap=[NSBitmapImageRep imageRepWithData:tiff];if(bitmap.pixelsWide<=8192 && bitmap.pixelsHigh<=8192 && (uint64_t)bitmap.pixelsWide*bitmap.pixelsHigh<=32*1024*1024)image=[bitmap representationUsingType:NSBitmapImageFileTypePNG properties:@{}];}}
    if(image && image.length<=4*1024*1024){if(lrd_clipboard_send(1,image.bytes,image.length))self.clipboardChange=pasteboard.changeCount;return;}
    NSString *text=[pasteboard stringForType:NSPasteboardTypeString];
    if(text){NSData *data=[text dataUsingEncoding:NSUTF8StringEncoding];if(data.length<=4*1024*1024 && lrd_clipboard_send(0,data.bytes,data.length))self.clipboardChange=pasteboard.changeCount;}
    else self.clipboardChange=pasteboard.changeCount;
}
- (void)receiveClipboard:(NSData *)data {
    if(!self.remoteConnected || !self.clipboardEnabled || data.length<33 || data.length>4*1024*1024+33)return;
    const uint8_t *b=data.bytes;NSData *content=[data subdataWithRange:NSMakeRange(33,data.length-33)];NSPasteboard *pasteboard=NSPasteboard.generalPasteboard;
    if(b[0]==0){NSString *text=[[NSString alloc] initWithData:content encoding:NSUTF8StringEncoding];if(!text)return;[pasteboard clearContents];[pasteboard setString:text forType:NSPasteboardTypeString];}
    else if(b[0]==1){NSBitmapImageRep *image=[NSBitmapImageRep imageRepWithData:content];if(!image || image.pixelsWide>8192 || image.pixelsHigh>8192 || (uint64_t)image.pixelsWide*image.pixelsHigh>32*1024*1024)return;[pasteboard clearContents];[pasteboard setData:content forType:NSPasteboardTypePNG];}
    self.clipboardChange=pasteboard.changeCount;
}

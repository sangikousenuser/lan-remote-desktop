- (void)transportMode:(NSMenuItem *)sender {
    if(self.remoteConnected){self.status.stringValue=@"通信方式は切断してから変更してください。";return;}
    if(sender.tag<0||sender.tag>2)return;
    lrd_set_transport((uint8_t)sender.tag);
    [[NSUserDefaults standardUserDefaults] setInteger:sender.tag forKey:@"transportMode"];
    for(NSMenuItem *item in sender.menu.itemArray)item.state=(item==sender)?NSControlStateValueOn:NSControlStateValueOff;
    self.status.stringValue=@"次回接続の通信方式を変更しました。接続できない場合はWindowsの限定Firewall規則を確認してください。";
}
- (void)reloadInterfaces {
    char *json=lrd_interfaces_json();
    NSData *data=json ? [[NSString stringWithUTF8String:json] dataUsingEncoding:NSUTF8StringEncoding] : nil;
    if (json) lrd_free_string(json);
    NSArray *interfaces=data ? [NSJSONSerialization JSONObjectWithData:data options:0 error:NULL] : @[];
    NSInteger selected=self.interfaces.selectedItem.tag;
    [self.interfaces removeAllItems];
    for (NSDictionary *nic in interfaces) {
        [self.interfaces addItemWithTitle:[NSString stringWithFormat:@"%@ — %@",nic[@"label"],nic[@"address"]]];
        self.interfaces.lastItem.tag=[nic[@"index"] integerValue];
    }
    [self.interfaces selectItemWithTag:selected];
    if (interfaces.count==0) { [self.interfaces addItemWithTitle:@"IPv4設定済みEthernetがありません"]; self.interfaces.lastItem.tag=0; }
}
- (void)discover:(id)sender {
    (void)sender; [self reloadInterfaces];
    if (self.interfaces.selectedItem.tag==0) {self.status.stringValue=@"有線アダプターを接続し、DHCPまたはLink-Localアドレスが付くまで待ってください。";return;}
    self.status.stringValue=@"選択したEthernet内でWindows Hostを探索しています…";
    lrd_discover((uint32_t)self.interfaces.selectedItem.tag);
}
- (void)connect:(id)sender {
    (void)sender;
    if(self.remoteConnected)[self disconnect:nil];
    if (self.interfaces.selectedItem.tag==0 || self.address.stringValue.length==0) {
        self.status.stringValue=@"Ethernetと接続先を指定してください。WindowsではHostを --serve で起動します。";return;
    }
    [self stop:nil];
    lrd_connect((uint32_t)self.interfaces.selectedItem.tag,self.address.stringValue.UTF8String);
}
- (void)disconnect:(id)sender {
    (void)sender;
    if (self.remoteConnected) [self sendInput:@{@"event":@"Release"}];
    [self releaseRelativeMouse];self.remoteConnected=NO;self.captureInput=NO;self.awaitingFrame=NO;self.previousModifiers=0;
    lrd_disconnect();self.networkGeneration++;[self stop:nil];
    if (sender) self.status.stringValue=@"手動で切断しました。自動再接続は行いません。";
}
- (void)recapture:(id)sender {
    (void)sender;if (!self.remoteConnected) return;
    if(!self.decoder || atomic_load(&self.decoder->decoded)==0){self.awaitingFrame=YES;self.status.stringValue=@"操作前に最初の映像を待っています。";return;}
    self.captureInput=YES;self.awaitingFrame=NO;self.previousModifiers=0;
    self.status.stringValue=@"入力を取得しました。⌃⌥⌘EscでMac側へ戻せます。";
}
- (void)relative:(id)sender {
    if (!self.remoteConnected) return;
    [self sendInput:@{@"event":@"Release"}];
    self.relativeMouse=!self.relativeMouse;
    if(self.relativeMouse){
        NSPoint center=[self.videoContainer convertPoint:NSMakePoint(self.videoContainer.bounds.size.width/2,self.videoContainer.bounds.size.height/2) toView:nil];
        NSRect screen=[self.window convertRectToScreen:NSMakeRect(center.x,center.y,0,0)];
        CGWarpMouseCursorPosition(CGPointMake(screen.origin.x,NSMaxY(NSScreen.screens.firstObject.frame)-screen.origin.y));
        if(CGAssociateMouseAndMouseCursorPosition(false)!=kCGErrorSuccess){self.relativeMouse=NO;self.status.stringValue=@"相対マウスを有効にできませんでした。";return;}
        [NSCursor hide];
    }else{CGAssociateMouseAndMouseCursorPosition(true);[NSCursor unhide];}
    if ([sender isKindOfClass:[NSMenuItem class]]) [(NSMenuItem *)sender setState:self.relativeMouse?NSControlStateValueOn:NSControlStateValueOff];
    self.status.stringValue=self.relativeMouse ? @"相対マウスモード。⌃⌥⌘Escで入力を解放します。" : @"通常マウスモードに戻しました。";
}
- (void)releaseRelativeMouse {if(self.relativeMouse){CGAssociateMouseAndMouseCursorPosition(true);[NSCursor unhide];self.relativeMouse=NO;}}
- (void)commandMode:(id)sender {
    if (self.remoteConnected) [self sendInput:@{@"event":@"Release"}];
    self.windowsCommand=!self.windowsCommand;self.previousModifiers=0;
    [(NSMenuItem *)sender setState:self.windowsCommand?NSControlStateValueOn:NSControlStateValueOff];
}
- (BOOL)sendInput:(NSDictionary *)input {
    NSData *data=[NSJSONSerialization dataWithJSONObject:input options:0 error:NULL];
    if (!data || !lrd_input_json(data.bytes,data.length)) {self.captureInput=NO;self.status.stringValue=@"入力送信を停止しました。接続状態を確認してください。";return NO;}
    return YES;
}
- (NSEvent *)handleInput:(NSEvent *)event {
    if((self.window.styleMask&NSWindowStyleMaskFullScreen)&&event.window==self.window){
        NSPoint point=event.locationInWindow;
        if(event.type==NSEventTypeMouseMoved)self.fullToolbar.hidden=point.y<self.window.contentView.bounds.size.height-52;
        if(!self.fullToolbar.hidden && NSPointInRect(point,self.fullToolbar.frame))return event;
    }
    if (!self.remoteConnected || event.window!=self.window) return event;
    NSUInteger flags=event.modifierFlags;
    if (event.type==NSEventTypeKeyDown && event.keyCode==53 &&
        (flags&(NSEventModifierFlagControl|NSEventModifierFlagOption|NSEventModifierFlagCommand))==(NSEventModifierFlagControl|NSEventModifierFlagOption|NSEventModifierFlagCommand)) {
        [self sendInput:@{@"event":@"Release"}];[self releaseRelativeMouse];self.captureInput=NO;self.awaitingFrame=NO;self.previousModifiers=0;
        self.status.stringValue=@"Mac側へ入力を解放しました。「入力を再取得」で戻れます。";return nil;
    }
    if(!self.captureInput)return event;
    if((event.type==NSEventTypeKeyDown || event.type==NSEventTypeKeyUp) && event.keyCode==15 && (flags&(NSEventModifierFlagControl|NSEventModifierFlagOption))==(NSEventModifierFlagControl|NSEventModifierFlagOption)) {if(event.type==NSEventTypeKeyDown)[self relative:nil];return nil;}
    if ((event.type==NSEventTypeKeyDown || event.type==NSEventTypeKeyUp) && event.keyCode==3 &&
        (flags&(NSEventModifierFlagControl|NSEventModifierFlagCommand))==(NSEventModifierFlagControl|NSEventModifierFlagCommand)) return event;
    if (event.type==NSEventTypeFlagsChanged) {
        const NSUInteger bits[]={NSEventModifierFlagShift,NSEventModifierFlagOption,NSEventModifierFlagControl|(self.windowsCommand?0:NSEventModifierFlagCommand),self.windowsCommand?NSEventModifierFlagCommand:0};
        const uint16_t codes[]={0x10,0x12,0x11,0x5b};
        for (NSUInteger i=0;i<4;i++) if (((self.previousModifiers&bits[i])!=0)!=((flags&bits[i])!=0)) [self sendInput:@{@"event":@"Key",@"code":@(codes[i]),@"down":((flags&bits[i]) ? @YES : @NO)}];
        if ((self.previousModifiers&NSEventModifierFlagCapsLock)!=(flags&NSEventModifierFlagCapsLock)) {
            [self sendInput:@{@"event":@"Key",@"code":@0x14,@"down":@YES}];[self sendInput:@{@"event":@"Key",@"code":@0x14,@"down":@NO}];
        }
        self.previousModifiers=flags;return nil;
    }
    if (event.type==NSEventTypeKeyDown || event.type==NSEventTypeKeyUp) {
        if (event.keyCode==102 || event.keyCode==104) {
            if (event.type==NSEventTypeKeyDown) [self sendInput:@{@"event":@"Ime",@"enabled":(event.keyCode==104 ? @YES : @NO)}];return nil;
        }
        if((event.keyCode==46)&&(flags&(NSEventModifierFlagControl|NSEventModifierFlagOption))==(NSEventModifierFlagControl|NSEventModifierFlagOption)){if(event.type==NSEventTypeKeyDown)[self displaySettings:nil];return nil;}
        self.jisKeyboard=KBGetLayoutType(LMGetKbdType())==kKeyboardJIS;
        uint16_t code=lrdWindowsKey(event.keyCode,self.jisKeyboard);
        if (code) [self sendInput:@{@"event":@"Key",@"code":@(code),@"down":(event.type==NSEventTypeKeyDown ? @YES : @NO)}];
        return nil;
    }
    NSPoint point=[self.videoContainer convertPoint:event.locationInWindow fromView:nil];
    if (!NSPointInRect(point,self.videoContainer.bounds)) {
        if (event.type==NSEventTypeLeftMouseUp || event.type==NSEventTypeRightMouseUp || event.type==NSEventTypeOtherMouseUp)
            [self sendInput:@{@"event":@"Button",@"button":@(MIN(4,event.buttonNumber)),@"down":@NO}];
        return event;
    }
    CGFloat scale=MIN(self.videoContainer.bounds.size.width/MAX(1,self.remoteWidth),self.videoContainer.bounds.size.height/MAX(1,self.remoteHeight));
    CGFloat width=self.remoteWidth*scale,height=self.remoteHeight*scale;
    CGFloat x=(point.x-(self.videoContainer.bounds.size.width-width)/2)/MAX(1,width);
    CGFloat y=1-(point.y-(self.videoContainer.bounds.size.height-height)/2)/MAX(1,height);
    x=MAX(0,MIN(1,x));y=MAX(0,MIN(1,y));
    switch (event.type) {
        case NSEventTypeMouseMoved: case NSEventTypeLeftMouseDragged: case NSEventTypeRightMouseDragged: case NSEventTypeOtherMouseDragged:
            if (self.relativeMouse) [self sendInput:@{@"event":@"Relative",@"dx":@((int)MAX(-32767,MIN(32767,event.deltaX))),@"dy":@((int)MAX(-32767,MIN(32767,event.deltaY)))}];
            else [self sendInput:@{@"event":@"Mouse",@"x":@(x),@"y":@(y)}];break;
        case NSEventTypeLeftMouseDown: case NSEventTypeLeftMouseUp: case NSEventTypeRightMouseDown: case NSEventTypeRightMouseUp: case NSEventTypeOtherMouseDown: case NSEventTypeOtherMouseUp:
            if(!self.relativeMouse)[self sendInput:@{@"event":@"Mouse",@"x":@(x),@"y":@(y)}];
            [self sendInput:@{@"event":@"Button",@"button":@(MIN(4,event.buttonNumber)),@"down":((event.type==NSEventTypeLeftMouseDown||event.type==NSEventTypeRightMouseDown||event.type==NSEventTypeOtherMouseDown) ? @YES : @NO)}];break;
        case NSEventTypeScrollWheel: [self sendInput:@{@"event":@"Wheel",@"delta":@((int)MAX(-12000,MIN(12000,event.scrollingDeltaY*120)))}];break;
        default:return event;
    }
    return nil;
}
- (void)windowDidResignKey:(NSNotification *)notification {
    (void)notification;
    if (self.remoteConnected) {[self sendInput:@{@"event":@"Release"}];[self releaseRelativeMouse];self.captureInput=NO;self.awaitingFrame=NO;self.previousModifiers=0;}
}
- (void)networkEvent:(uint32_t)kind data:(NSData *)data {
    if(kind==9){NSDictionary *stats=[NSJSONSerialization JSONObjectWithData:data options:0 error:NULL];self.rttMilliseconds=[stats[@"rtt_ms"] doubleValue];self.receivedBytes=[stats[@"received_bytes"] unsignedLongLongValue];self.sentBytes=[stats[@"sent_bytes"] unsignedLongLongValue];return;}
    if (kind==4) {[self receiveClipboard:data];return;}
    if (kind==8) {if(self.remoteConnected && self.audioEnabled)[self.audio play:data];return;}
    if (kind==5) {NSString *message=[[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];if(message)self.transferStatus.stringValue=message;return;}
    if (kind==1) {NSString *status=[[NSString alloc] initWithData:data encoding:NSUTF8StringEncoding];if(status)self.status.stringValue=status;return;}
    if (kind==6) {
        NSArray *found=[NSJSONSerialization JSONObjectWithData:data options:0 error:NULL];
        if (found.count==0) {self.status.stringValue=@"Hostが見つかりません。Windowsの --serve、ファイアウォール、有線アドレスを確認してください。";return;}
        NSPopUpButton *list=[[NSPopUpButton alloc] initWithFrame:NSMakeRect(0,0,420,30) pullsDown:NO];
        for (NSArray *entry in found) {if (entry.count!=2)continue;NSDictionary *host=entry[1];[list addItemWithTitle:[NSString stringWithFormat:@"%@ — %@",host[@"name"],entry[0]]];list.lastItem.representedObject=entry[0];}
        NSAlert *alert=[NSAlert new];alert.messageText=@"接続先を選択";alert.informativeText=@"探索結果は未認証です。接続時に端末鍵と承認を確認します。";alert.accessoryView=list;
        [alert addButtonWithTitle:@"選択"];[alert addButtonWithTitle:@"キャンセル"];
        if ([alert runModal]==NSAlertFirstButtonReturn) self.address.stringValue=list.selectedItem.representedObject;
        self.status.stringValue=@"接続先を選びました。「接続」で認証と承認を開始します。";return;
    }
    if (kind==2) {
        NSDictionary *config=[NSJSONSerialization JSONObjectWithData:data options:0 error:NULL];
        BOOL firstConnection=!self.remoteConnected;
        self.monitors=config[@"monitors"];self.currentMonitor=[config[@"monitor"] integerValue];
        [self stop:nil];self.decoder=[LRDDecoder new];self.decoder.allowSoftware=self.forceSoftware;
        self.remoteWidth=[config[@"width"] unsignedIntValue];self.remoteHeight=[config[@"height"] unsignedIntValue];
        self.started=CACurrentMediaTime();self.elapsed=0;[self prepareRenderer];
        self.remoteConnected=YES;self.captureInput=NO;self.awaitingFrame=YES;self.previousModifiers=0;self.lastDecodedCount=0;self.lastDecodedAt=CACurrentMediaTime();
        if(firstConnection && !(self.window.styleMask&NSWindowStyleMaskFullScreen))[self.window toggleFullScreen:nil];
        self.clipboardChange=NSPasteboard.generalPasteboard.changeCount-1;
        self.clipboardTimer=[NSTimer scheduledTimerWithTimeInterval:0.25 repeats:YES block:^(NSTimer *t){(void)t;[self pollClipboard];}];
        [self sendControl:@{@"clipboard":self.clipboardEnabled?@YES:@NO,@"audio":self.audioEnabled?@1:@0}];
        self.metricsTimer=[NSTimer scheduledTimerWithTimeInterval:0.5 repeats:YES block:^(NSTimer *timer){(void)timer;[self updateMetrics];}];
        return;
    }
    if (kind==7) {[self releaseRelativeMouse];self.remoteConnected=NO;self.captureInput=NO;self.awaitingFrame=NO;self.previousModifiers=0;[self stop:nil];return;}
}

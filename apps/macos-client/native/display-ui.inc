- (void)refreshDisplayModes:(id)sender {
    (void)sender;[self.displayModes removeAllItems];
    [self.displayModes addItemWithTitle:@"現在のWindows解像度を維持"];
    NSDictionary *monitor=self.displayMonitors.selectedItem.representedObject;
    for(NSDictionary *mode in monitor[@"modes"]){
        [self.displayModes addItemWithTitle:[NSString stringWithFormat:@"%@ × %@  /  %@ Hz",mode[@"width"],mode[@"height"],mode[@"hz"]]];
        self.displayModes.lastItem.representedObject=mode;
    }
}
- (void)displaySettings:(id)sender {
    (void)sender;if(!self.remoteConnected)return;
    self.automaticDisplayPending=NO;
    [self sendInput:@{@"event":@"Release"}];[self releaseRelativeMouse];self.captureInput=NO;
    NSView *view=[[NSView alloc] initWithFrame:NSMakeRect(0,0,450,100)];
    self.displayMonitors=[[NSPopUpButton alloc] initWithFrame:NSMakeRect(0,64,450,30) pullsDown:NO];
    self.displayModes=[[NSPopUpButton alloc] initWithFrame:NSMakeRect(0,24,450,30) pullsDown:NO];
    for(NSDictionary *m in self.monitors){[self.displayMonitors addItemWithTitle:[NSString stringWithFormat:@"%@ — %@ × %@",m[@"name"],m[@"width"],m[@"height"]]];self.displayMonitors.lastItem.representedObject=m;self.displayMonitors.lastItem.tag=[m[@"id"] integerValue];}
    [self.displayMonitors selectItemWithTag:self.currentMonitor];self.displayMonitors.target=self;self.displayMonitors.action=@selector(refreshDisplayModes:);[self refreshDisplayModes:nil];
    [view addSubview:self.displayMonitors];[view addSubview:self.displayModes];
    NSScreen *screen=self.window.screen?:NSScreen.mainScreen;CGFloat scale=screen.backingScaleFactor;
    NSAlert *alert=[NSAlert new];alert.messageText=@"モニターと表示モード";
    alert.informativeText=[NSString stringWithFormat:@"Mac: %.0f × %.0f pt / %.0f × %.0f pixels\nWindowsが対応するモードだけを表示しています。比率を保って描画します。変更は切断後に復元します。",screen.frame.size.width,screen.frame.size.height,screen.frame.size.width*scale,screen.frame.size.height*scale];alert.accessoryView=view;
    [alert addButtonWithTitle:@"適用"];[alert addButtonWithTitle:@"キャンセル"];
    if([alert runModal]==NSAlertFirstButtonReturn){
        NSMutableDictionary *request=[@{@"monitor":@(self.displayMonitors.selectedItem.tag)} mutableCopy];
        if(self.displayModes.selectedItem.representedObject)request[@"mode"]=self.displayModes.selectedItem.representedObject;
        [self sendControl:@{@"display":request}];
    }else [self recapture:nil];
}
- (void)automaticDisplay:(NSMenuItem *)sender {
    BOOL disabled=[[NSUserDefaults standardUserDefaults] boolForKey:@"keepWindowsResolution"];
    [[NSUserDefaults standardUserDefaults] setBool:!disabled forKey:@"keepWindowsResolution"];
    sender.state=disabled?NSControlStateValueOn:NSControlStateValueOff;
    self.automaticDisplayPending=disabled&&self.remoteConnected;
}
- (void)applyAutomaticDisplay {
    if(!self.automaticDisplayPending||!self.remoteConnected||!self.decoder||atomic_load(&self.decoder->presented)<2||CACurrentMediaTime()-self.started<1.||self.lastDecodeDurationMs<=0||self.lastRenderDurationMs<=0)return;
    self.automaticDisplayPending=NO;
    NSDictionary *monitor=nil;for(NSDictionary *candidate in self.monitors)if([candidate[@"id"] integerValue]==self.currentMonitor){monitor=candidate;break;}
    if(!monitor)return;
    NSScreen *screen=self.window.screen?:NSScreen.mainScreen;if(!screen)return;
    double hz=60.;NSNumber *displayNumber=screen.deviceDescription[@"NSScreenNumber"];
    if(displayNumber){CGDisplayModeRef mode=CGDisplayCopyDisplayMode(displayNumber.unsignedIntValue);if(mode){double rate=CGDisplayModeGetRefreshRate(mode);if(isfinite(rate)&&rate>=1)hz=rate;CGDisplayModeRelease(mode);}}
    NSDictionary *capabilities=@{@"physical_width":@((uint32_t)(screen.frame.size.width*screen.backingScaleFactor)),@"physical_height":@((uint32_t)(screen.frame.size.height*screen.backingScaleFactor)),
        @"logical_width":@((uint32_t)screen.frame.size.width),@"logical_height":@((uint32_t)screen.frame.size.height),
        @"current_width":@(self.remoteWidth),@"current_height":@(self.remoteHeight),@"refresh_hz":@(hz),
        @"decode_ms":@(self.lastDecodeDurationMs),
        @"render_ms":@(self.lastRenderDurationMs),
        @"hardware":@(atomic_load(&self.decoder->hardware)),@"modes":monitor[@"modes"]?:@[]};
    NSData *request=[NSJSONSerialization dataWithJSONObject:capabilities options:0 error:NULL];if(!request)return;
    char *raw=lrd_choose_display_json(request.bytes,request.length);if(!raw)return;
    NSString *text=[NSString stringWithUTF8String:raw];lrd_free_string(raw);NSData *data=[text dataUsingEncoding:NSUTF8StringEncoding];
    NSDictionary *mode=data?[NSJSONSerialization JSONObjectWithData:data options:0 error:NULL]:nil;
    if(![mode isKindOfClass:[NSDictionary class]]||([mode[@"width"] unsignedIntValue]==self.remoteWidth&&[mode[@"height"] unsignedIntValue]==self.remoteHeight))return;
    [self sendInput:@{@"event":@"Release"}];[self releaseRelativeMouse];self.captureInput=NO;self.awaitingFrame=YES;
    [self sendControl:@{@"display":@{@"monitor":@(self.currentMonitor),@"mode":mode}}];
    self.status.stringValue=@"Macの表示サイズと復号負荷に合わせ、Windowsが対応する解像度へ調整しています。切断後は元のモードへ戻します。";
}
- (void)windowDidEnterFullScreen:(NSNotification *)notification {
    (void)notification;self.normalVideoFrame=self.videoContainer.frame;
    for(NSView *v in self.window.contentView.subviews)if(v!=self.videoContainer)v.hidden=YES;
    self.videoContainer.frame=self.window.contentView.bounds;
    if(!self.fullToolbar){
        self.fullToolbar=[[NSView alloc] initWithFrame:NSMakeRect(0,self.window.contentView.bounds.size.height-48,self.window.contentView.bounds.size.width,48)];
        self.fullToolbar.wantsLayer=YES;self.fullToolbar.layer.backgroundColor=[NSColor colorWithCalibratedWhite:0.12 alpha:0.96].CGColor;
        self.fullToolbar.autoresizingMask=NSViewWidthSizable|NSViewMinYMargin;
        NSArray *titles=@[@"入力を解放",@"入力を再取得",@"モニター…",@"ファイル…",@"切断",@"全画面を終了"];
        SEL actions[]={@selector(escapeInput:),@selector(recapture:),@selector(displaySettings:),@selector(files:),@selector(disconnect:),@selector(fullscreen:)};
        for(NSUInteger i=0;i<titles.count;i++){NSButton *b=[NSButton buttonWithTitle:titles[i] target:self action:actions[i]];b.frame=NSMakeRect(12+146*i,8,140,32);[self.fullToolbar addSubview:b];}
        [self.window.contentView addSubview:self.fullToolbar];
    }
    self.fullToolbar.hidden=NO;
}
- (void)windowDidExitFullScreen:(NSNotification *)notification {
    (void)notification;
    for(NSView *v in self.window.contentView.subviews)v.hidden=NO;
    self.fullToolbar.hidden=YES;
    self.videoContainer.frame=NSMakeRect(28,168,self.window.contentView.bounds.size.width-56,self.window.contentView.bounds.size.height-316);
}
- (void)windowDidResize:(NSNotification *)notification {
    (void)notification;if(self.window.styleMask&NSWindowStyleMaskFullScreen)self.videoContainer.frame=self.window.contentView.bounds;
}
- (void)escapeInput:(id)sender {
    (void)sender;if(self.remoteConnected)[self sendInput:@{@"event":@"Release"}];[self releaseRelativeMouse];self.captureInput=NO;self.awaitingFrame=NO;self.previousModifiers=0;
    self.status.stringValue=@"Mac側へ入力を解放しました。「入力を再取得」で戻れます。";
}

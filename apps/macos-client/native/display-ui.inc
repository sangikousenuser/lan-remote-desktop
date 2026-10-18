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

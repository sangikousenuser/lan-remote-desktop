- (void)dashboard:(id)sender {
    (void)sender;
    if(!self.dashboard){
        self.dashboard=[[NSPanel alloc] initWithContentRect:NSMakeRect(0,0,680,410) styleMask:NSWindowStyleMaskTitled|NSWindowStyleMaskClosable backing:NSBackingStoreBuffered defer:NO];
        self.dashboard.title=@"LAN Remote Desktop — 性能";self.dashboard.releasedWhenClosed=NO;
        self.dashboardText=label(@"接続または映像テストの開始後に数値を表示します。",13,NO);
        self.dashboardText.frame=NSMakeRect(20,20,640,370);self.dashboardText.font=[NSFont userFixedPitchFontOfSize:13];
        [self.dashboard.contentView addSubview:self.dashboardText];[self.dashboard center];
    }
    [self updateMetrics];[self.dashboard makeKeyAndOrderFront:nil];
}
- (void)updateDashboard {
    if(!self.dashboard.isVisible)return;
    NSString *(^number)(id)=^NSString *(id value){return [value isKindOfClass:[NSNumber class]]?[NSString stringWithFormat:@"%.2f",[value doubleValue]]:@"—";};
    NSDictionary *local=self.resourceStatistics[@"metrics"],*remote=self.remoteStatistics;
    if(![local isKindOfClass:[NSDictionary class]])local=@{};if(![remote isKindOfClass:[NSDictionary class]])remote=@{};
    self.dashboardText.stringValue=[NSString stringWithFormat:
        @"RTT                         %.2f ms\n"
         "Windows capture / encode    %@ / %@ ms\n"
         "Mac decode / render         %@ / %@ ms\n"
         "Mac GPU command             %@ ms（10.15以降の対応GPU）\n"
         "capture / decode / render   %@ / %@ / %@ fps\n"
         "Mac受信bitrate              %@ bps\n"
         "Mac file送出 / 検証済受信   %@ / %@ bytes/s\n"
         "Mac CPU / RAM               %@ %% / %@ MiB\n"
         "Windows CPU / RAM           %@ %% / %@ MiB\n"
         "復帰 / 欠落 / 復号エラー    %@ / %@ / %@\n"
         "GPU使用率                   —（利用可能な取得APIなし）\n\n"
         "renderはCPU描画からcommand完了まで。画面の光学表示遅延ではありません。\n"
         "file送出は送信queueへ受理された本文、受信はhash検証済み本文です。",
        self.rttMilliseconds,number(remote[@"capture_ms"]),number(remote[@"encode_ms"]),number(local[@"decode_ms"]),number(local[@"render_ms"]),number(local[@"render_gpu_ms"]),
        number(remote[@"capture_fps"]),number(local[@"decode_fps"]),number(local[@"render_fps"]),number(local[@"transport_bitrate_bps"]),
        number(local[@"file_send_bytes_per_second"]),number(local[@"file_receive_bytes_per_second"]),number(self.resourceStatistics[@"cpu_percent"]),([self.resourceStatistics[@"resident_bytes"] isKindOfClass:[NSNumber class]]?number(@([self.resourceStatistics[@"resident_bytes"] doubleValue]/1048576.)):@"—"),
        number(remote[@"cpu_percent"]),([remote[@"resident_bytes"] isKindOfClass:[NSNumber class]]?number(@([remote[@"resident_bytes"] doubleValue]/1048576.)):@"—"),
        number(self.resourceStatistics[@"reconnects"]),number(local[@"dropped_frames"]),number(local[@"decode_errors"])];
}

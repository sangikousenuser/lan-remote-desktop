- (void)agent:(id)sender {
    NSString *path=[NSHomeDirectory() stringByAppendingPathComponent:@"Library/LaunchAgents/local.lanremotedesktop.agent.plist"];
    NSString *domain=[NSString stringWithFormat:@"gui/%u",getuid()];
    BOOL enabled=[[NSFileManager defaultManager] fileExistsAtPath:path];
    if(enabled){
        NSTask *task=[NSTask new];task.launchPath=@"/bin/launchctl";task.arguments=@[@"bootout",domain,path];[task launch];[task waitUntilExit];
        [[NSFileManager defaultManager] removeItemAtPath:path error:NULL];
        self.status.stringValue=@"ログイン時のEthernet監視を解除しました。";
    }else{
        if(self.interfaces.selectedItem.tag==0 || !lrd_agent_configure((uint32_t)self.interfaces.selectedItem.tag)){self.status.stringValue=@"監視するEthernetを接続して選択してください。";return;}
        [[NSFileManager defaultManager] createDirectoryAtPath:path.stringByDeletingLastPathComponent withIntermediateDirectories:YES attributes:@{NSFilePosixPermissions:@0700} error:NULL];
        NSDictionary *plist=@{@"Label":@"local.lanremotedesktop.agent",@"ProgramArguments":@[[NSBundle mainBundle].executablePath,@"--agent"],@"RunAtLoad":@YES,@"KeepAlive":@YES,@"ThrottleInterval":@10,@"ProcessType":@"Background"};
        NSData *data=[NSPropertyListSerialization dataWithPropertyList:plist format:NSPropertyListXMLFormat_v1_0 options:0 error:NULL];
        if(![data writeToFile:path options:NSDataWritingAtomic error:NULL]){self.status.stringValue=@"LaunchAgentを保存できませんでした。";return;}
        [[NSFileManager defaultManager] setAttributes:@{NSFilePosixPermissions:@0600} ofItemAtPath:path error:NULL];
        NSTask *task=[NSTask new];task.launchPath=@"/bin/launchctl";task.arguments=@[@"bootstrap",domain,path];[task launch];[task waitUntilExit];
        if(task.terminationStatus!=0){[[NSFileManager defaultManager] removeItemAtPath:path error:NULL];self.status.stringValue=@"監視を登録できませんでした。アプリをApplicationsへ移動して再試行してください。";return;}
        self.status.stringValue=@"選択Ethernetの監視を登録しました。接続先発見後にアプリを開き、接続を確認します。";
    }
    if([sender isKindOfClass:[NSMenuItem class]])[(NSMenuItem*)sender setState:enabled?NSControlStateValueOff:NSControlStateValueOn];
}
- (void)agentNotification:(NSNotification *)notification {
    if(self.remoteConnected)return;
    NSDictionary *info=notification.userInfo;
    if(![info[@"index"] isKindOfClass:[NSNumber class]] || ![info[@"address"] isKindOfClass:[NSString class]] || [info[@"address"] length]>128)return;
    [self reloadInterfaces];[self.interfaces selectItemWithTag:[info[@"index"] integerValue]];
    if(self.interfaces.selectedItem.tag!=[info[@"index"] integerValue])return;
    self.address.stringValue=info[@"address"];[self.window makeKeyAndOrderFront:nil];[NSApp activateIgnoringOtherApps:YES];[self connect:nil];
}

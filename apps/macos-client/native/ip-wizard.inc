- (NSString *)ipHelper {
    NSString *path=[NSBundle.mainBundle.bundlePath stringByAppendingPathComponent:@"Contents/Helpers/lrd-network-helper"];
    return [NSFileManager.defaultManager isExecutableFileAtPath:path]?path:nil;
}
- (NSDictionary *)ipHelperStatus {
    NSString *helper=[self ipHelper];if(!helper)return nil;
    NSTask *task=[NSTask new];task.launchPath=helper;task.arguments=@[@"--status"];
    NSPipe *pipe=[NSPipe pipe];task.standardOutput=pipe;task.standardError=[NSFileHandle fileHandleForWritingAtPath:@"/dev/null"];
    @try{[task launch];NSData *data=[pipe.fileHandleForReading readDataToEndOfFile];[task waitUntilExit];if(task.terminationStatus||data.length>4096)return nil;id value=[NSJSONSerialization JSONObjectWithData:data options:0 error:NULL];return [value isKindOfClass:NSDictionary.class]?value:nil;}@catch(NSException *exception){(void)exception;return nil;}
}
- (void)checkIPRecovery {
    NSDictionary *status=[self ipHelperStatus];NSString *phase=status[@"phase"];if(![phase isKindOfClass:NSString.class])return;
    BOOL pending=[@[@"Active",@"Starting",@"Restoring",@"Deferred"] containsObject:phase];
    if(pending && NSDate.date.timeIntervalSince1970-[status[@"updated"] doubleValue]>30){
        self.status.stringValue=@"一時固定IPの前回状態を確認してください。設定画面の「復元」でOS承認後に照合・復元できます。";
        [self networkWizard:nil];
    }
}
- (void)networkWizard:(id)sender {
    (void)sender;
    if(self.remoteConnected){self.status.stringValue=@"一時固定IPを設定する前にリモート接続を切断してください。";return;}
    if(self.ipHelperTimer){self.status.stringValue=@"OSの承認または固定IP設定の完了を待っています。";return;}
    NSString *helper=[self ipHelper];if(!helper){self.status.stringValue=@"IP設定helperが見つかりません。配布用.appから起動してください。";return;}
    char *json=lrd_adapters_json();NSData *data=json?[[NSString stringWithUTF8String:json] dataUsingEncoding:NSUTF8StringEncoding]:nil;if(json)lrd_free_string(json);
    id value=data?[NSJSONSerialization JSONObjectWithData:data options:0 error:NULL]:nil;NSArray *adapters=[value isKindOfClass:NSArray.class]?value:@[];
    NSPopUpButton *list=[[NSPopUpButton alloc] initWithFrame:NSMakeRect(0,0,440,30) pullsDown:NO];
    for(NSDictionary *adapter in adapters){[list addItemWithTitle:[NSString stringWithFormat:@"%@ (%@) — %@",adapter[@"label"],adapter[@"name"],[adapter[@"active"] boolValue]?@"リンクあり":@"未接続"]];list.lastItem.representedObject=adapter;}
    if(!adapters.count)[list addItemWithTitle:@"物理Ethernetがありません（復元の照合は可能）"];
    NSAlert *alert=[NSAlert new];alert.messageText=@"有線LANの一時固定IP";
    alert.informativeText=@"DHCP／Link-Localで接続できない場合にだけ使用します。Macには10.254.240.2/30を追加します。Windows側でも同じ設定画面から10.254.240.1/30を設定してください。\n\n選択したEthernetだけが対象です。既存アドレス・DNS・Wi-Fiの設定は保持します。衝突を検査し、抜線・取消・12時間後に一時アドレスを除去します。設定と復元にはOSの管理者承認を使用します。";
    alert.accessoryView=list;[alert addButtonWithTitle:@"選択したEthernetへ適用"];[alert addButtonWithTitle:@"前回の一時IPを復元／権限を終了"];[alert addButtonWithTitle:@"キャンセル"];
    NSModalResponse response=[alert runModal];if(response!=NSAlertFirstButtonReturn && response!=NSAlertSecondButtonReturn)return;
    BOOL restore=response==NSAlertSecondButtonReturn;NSDictionary *adapter=list.selectedItem.representedObject;
    uint32_t index=[adapter[@"index"] unsignedIntValue];NSString *mac=adapter[@"mac"];
    if(!restore && (!index || ![adapter[@"active"] boolValue] || ![mac isKindOfClass:NSString.class] || mac.length!=17 || [mac rangeOfCharacterFromSet:[[NSCharacterSet characterSetWithCharactersInString:@"0123456789abcdefABCDEF:"] invertedSet]].location!=NSNotFound)){self.status.stringValue=@"接続中の物理Ethernetを選んでください。";return;}
    NSString *quoted=[NSString stringWithFormat:@"'%@'",[helper stringByReplacingOccurrencesOfString:@"'" withString:@"'\\''"]];
    NSString *arguments=restore?@"--restore":[NSString stringWithFormat:@"--apply --interface %u --mac %@",index,mac];
    NSString *command=[NSString stringWithFormat:@"%@ %@ </dev/null >/dev/null 2>&1 &",quoted,arguments];
    self.status.stringValue=@"macOSの管理者承認を待っています…";
    // The OS authorizes this fixed helper invocation. It retains no reusable
    // application authorization token and listens on no privileged IPC port.
    self.ipHelperTimer=[NSTimer timerWithTimeInterval:1 repeats:NO block:^(NSTimer *timer){(void)timer;}];
    dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED,0),^{
        NSTask *task=[NSTask new];task.launchPath=@"/usr/bin/osascript";
        task.arguments=@[@"-e",@"on run argv\n do shell script (item 1 of argv) with administrator privileges\nend run",command];
        task.standardOutput=[NSFileHandle fileHandleForWritingAtPath:@"/dev/null"];task.standardError=[NSFileHandle fileHandleForWritingAtPath:@"/dev/null"];
        BOOL launched=YES;@try{[task launch];[task waitUntilExit];launched=task.terminationStatus==0;}@catch(NSException *exception){(void)exception;launched=NO;}
        dispatch_async(dispatch_get_main_queue(),^{
            self.ipHelperTimer=nil;
            if(!launched){self.status.stringValue=@"管理者承認が取り消されたか、helperを起動できませんでした。";return;}
            __block NSUInteger polls=0;
            self.ipHelperTimer=[NSTimer scheduledTimerWithTimeInterval:1 repeats:YES block:^(NSTimer *timer){
                NSDictionary *status=[self ipHelperStatus];NSString *phase=[status[@"phase"] isKindOfClass:NSString.class]?status[@"phase"]:@"";polls++;
                BOOL addressReady=NO;char *current=lrd_adapters_json();NSData *currentData=current?[[NSString stringWithUTF8String:current] dataUsingEncoding:NSUTF8StringEncoding]:nil;if(current)lrd_free_string(current);id currentAdapters=currentData?[NSJSONSerialization JSONObjectWithData:currentData options:0 error:NULL]:nil;
                if([currentAdapters isKindOfClass:NSArray.class])for(NSDictionary *nic in currentAdapters)if([nic[@"mac"] isEqual:mac])for(NSArray *ip in nic[@"addresses"])if(ip.count==2 && [ip[0] isEqual:@"10.254.240.2"] && [ip[1] integerValue]==30)addressReady=YES;
                BOOL done=restore?[phase isEqualToString:@"Idle"]:([phase isEqualToString:@"Active"]&&addressReady);
                BOOL failed=[@[@"Failed",@"Deferred"] containsObject:phase];
                if(done||failed||polls>=30){[timer invalidate];self.ipHelperTimer=nil;[self reloadInterfaces];self.status.stringValue=done?(restore?@"一時IPを復元しました。helperの権限は終了しています。":@"Mac側の一時IPを設定しました。Windows側も設定したら「探索」を実行してください。"):@"一時IPを設定・復元できませんでした。アドレス衝突、選択NIC、既存のIPv4や経路を確認してください。復元を保留した場合は手動変更を確認して再実行してください。";}
                else self.status.stringValue=restore?@"一時IPを照合・復元しています…":@"有線アドレスと経路の衝突を検査しています…";
            }];
        });
    });
}

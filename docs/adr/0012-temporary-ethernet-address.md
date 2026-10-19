# 0012: 管理者承認付き一時Ethernetアドレス

日付: 2026-10-19。対象: Q45/Q49/Q60。

DHCP/Link-Localで接続できない場合のローカルウィザードと、独立した `lrd-network-helper` を追加する。IPv4取得済みのNICは変更しない。両端でそれぞれNICを選び、Windows `10.254.240.1/30`、Mac `10.254.240.2/30` を設定する。IP通信前に対向端末へ設定を指示できるとは扱わない。

- 操作はEthernet一覧、状態参照、指定index+MACへの適用、記録したアドレスの復元だけ。任意IP、DNS、ゲートウェイ、パス、シェルコマンドを要求に含めない。Host/Clientは通常ユーザー権限のまま、選択画面の確認後にWindows `runas` / macOSの管理者承認付き `do shell script` でhelperを起動する。汎用の管理IPCや保存済みのアプリ権限tokenを設けず、設定/復元の呼出しごとにOS承認を使う安全側fallbackとする。
- 記録した物理Ethernet以外は変更しない。列挙時に物理有線を検査し、適用直前にMAC/indexを照合する。WindowsはGetIfEntry2の物理種別・MACを再検査しLUIDで追加/削除する。全NICのIPv4と既存non-default routeが/30と重なる場合は保留する。
- Macは選択デバイスへ拘束したBPFでRFC 5227のARP probeを3回送り、他端末による所有や同時probeを検出したら保留する。BPFはARPのみ、最大128bytesにfilterし記録しない。衝突がなければOS同梱 `/sbin/ifconfig` をシェルなし・固定引数で呼び出しaliasを追加する。WindowsはCreateUnicastIpAddressEntryのtentative DADを使用し、preferredになるまで成功を通知しない。DAD無効、duplicate、15秒timeoutは復元する。[Microsoft公式のDAD・非永続アドレスの仕様](https://learn.microsoft.com/en-us/windows/win32/api/netioapi/nf-netioapi-createunicastipaddressentry)に従い、optimistic DADを指定しない。
- DHCPモード、既存IP、DNS、デフォルトルートを変更しない。追加によるconnected /30 routeはOSに任せ、削除時に消える。旧設定の列挙と追加予定を**変更前に**永続化する。Macはroot所有・書換え禁止の親配下の0600 journal、WindowsはHKLM管理者領域。状態表示だけを一般ユーザーから参照可能にする。
- helperは1台で同時に1 leaseをOS lock/mutexで保持。500msごとに物理リンク・取消・アドレス・DHCP取得を照合し、抜線、取消、DHCP復帰、12時間で追加アドレスだけを除去して終了する。WindowsはOS上のアドレス寿命も12時間に設定する。通常の終了signalも復元を試す。権限取り消しは専用の管理者承認付き復元から行い、常駐権限サービスを残さない。
- 異常終了のsnapshotは次回起動で状態を案内し、管理者承認後に照合・復元する。MACで再識別するのでNIC番号変更は別NICへの書込みにならない。USB NIC自体が不在ならjournalを残す。prefix等の手動変更は無条件に書き戻さず保留し、別の追加IPを削除しない。電源断後の一時IPはOSの非永続設定として消える。
- Windows helperは版付きのファイル名で同梱し、lease監視中の実行ファイルを更新対象にしない。Mac helperは.appのsealへ含め、bundle単位の更新に同梱する。

自動試験はfake OS backendで、衝突/DAD失敗、journal書込み失敗、適用後の状態保存失敗、取消、NIC番号再割当て、不在/曖昧なMAC、手動prefix変更、他の追加IP保持、tentative行の照合を検証する。ARP parserは実Cコードで短いpacket、自己packet、所有claim、同時probeを検証する。実ネットワーク変更とOS承認UIは開発端末では実行していない。最後の実機受入で、両端の抜線・DHCP・衝突・再起動・Wi-Fi/DNS保持を確認する。

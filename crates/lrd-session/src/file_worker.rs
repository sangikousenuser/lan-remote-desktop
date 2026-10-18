use crate::{
    channel::Sender,
    transfer::{self, Service, Source},
    wire::Packet,
    Result,
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        mpsc::{self, SyncSender},
        Arc, Mutex, OnceLock,
    },
};
static SERVICES: OnceLock<Mutex<HashMap<String, Arc<Mutex<Service>>>>> = OnceLock::new();
#[derive(Clone)]
pub struct Files {
    pub service: Arc<Mutex<Service>>,
    pub incoming: SyncSender<Packet>,
    sender: Sender,
    update: Arc<dyn Fn(String) + Send + Sync>,
}
pub fn start(
    peer: String,
    sender: Sender,
    update: impl Fn(String) + Send + Sync + 'static,
) -> Result<Files> {
    let root = transfer::downloads()?;
    let mut services = SERVICES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| "transfer lock poisoned")?;
    let service = if let Some(service) = services.get(&peer) {
        service.clone()
    } else {
        if services.len() >= 32 {
            return crate::fail("peer transfer cache limit");
        }
        let journal = crate::identity::data_directory()?
            .join("outgoing")
            .join(&peer);
        let service = Arc::new(Mutex::new(
            Service::new(root, peer.clone()).persistent(&journal)?,
        ));
        services.insert(peer, service.clone());
        service
    };
    drop(services);
    let update: Arc<dyn Fn(String) + Send + Sync> = Arc::new(update);
    let (tx, rx) = mpsc::sync_channel::<Packet>(8);
    let worker_service = service.clone();
    let worker_sender = sender.clone();
    let worker_update = update.clone();
    std::thread::spawn(move || {
        while let Ok(packet) = rx.recv() {
            if worker_sender.stopped() {
                break;
            }
            let result = worker_service
                .lock()
                .map_err(|_| "transfer lock poisoned")
                .and_then(|mut s| {
                    s.handle(packet, &worker_sender).map_err(|_| {
                        "転送要求を拒否しました。パス・ハッシュ・転送状態を確認してください"
                    })
                });
            match result {
                Ok(message) => worker_update(message),
                Err(message) => worker_update(message.into()),
            }
        }
    });
    let files = Files {
        service,
        incoming: tx,
        sender,
        update,
    };
    let resume = files.clone();
    std::thread::spawn(move || {
        let offers = resume
            .service
            .lock()
            .map_err(|_| ())
            .and_then(|service| service.resume_offers().map_err(|_| ()));
        match offers {
            Ok(offers) => {
                for p in offers {
                    if resume.sender.send_bulk(p).is_err() {
                        break;
                    }
                }
            }
            Err(()) => (resume.update)("Source changed; cancel and select again".into()),
        }
    });
    Ok(files)
}
impl Files {
    pub fn send(&self, path: PathBuf) {
        let files = self.clone();
        std::thread::spawn(move || {
            (files.update)("転送元を検証しています…".into());
            let result = (|| -> Result<()> {
                let source = Source::build(&path)?;
                if files.sender.stopped() {
                    return Ok(());
                }
                let packet = files
                    .service
                    .lock()
                    .map_err(|_| "transfer lock poisoned")?
                    .offer_source(source)?;
                files.sender.send_bulk(packet)?;
                Ok(())
            })();
            if result.is_err() {
                (files.update)(
                    "送信できません。通常ファイル/フォルダーと転送元の変更を確認してください"
                        .into(),
                );
            }
        });
    }
    pub fn cancel(&self) {
        if let Ok(mut s) = self.service.lock() {
            if let Ok(packets) = s.cancel_all() {
                for p in packets {
                    let _ = self.sender.send_bulk(p);
                }
            }
        }
        (self.update)("転送をキャンセルしました".into());
    }
}

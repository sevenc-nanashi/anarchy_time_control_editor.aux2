use aviutl2_eframe::egui;

#[derive(Debug)]
pub enum WatcherMessage {
    ObjectChanged,
    ObjectSelected,
    Shutdown,
}

pub struct WatcherThread {
    _thread: Option<std::thread::JoinHandle<()>>,
    sender: std::sync::mpsc::Sender<WatcherMessage>,
}

struct WatcherThreadInternal {
    receiver: std::sync::mpsc::Receiver<WatcherMessage>,
}

impl WatcherThread {
    pub fn start() -> Self {
        let (sender, receiver) = std::sync::mpsc::channel::<WatcherMessage>();
        let thread = std::thread::spawn(move || {
            let internal = WatcherThreadInternal { receiver };
            internal.run();
        });
        Self {
            _thread: Some(thread),
            sender,
        }
    }

    pub fn notify_object_change(&self) {
        if let Err(e) = self.sender.send(WatcherMessage::ObjectChanged) {
            tracing::error!(
                "Failed to send object change message to watcher thread: {:?}",
                e
            );
        }
    }

    pub fn notify_object_selected(&self) {
        if let Err(e) = self.sender.send(WatcherMessage::ObjectSelected) {
            tracing::error!(
                "Failed to send object selected message to watcher thread: {:?}",
                e
            );
        }
    }
}
impl Drop for WatcherThread {
    fn drop(&mut self) {
        if let Err(e) = self.sender.send(WatcherMessage::Shutdown) {
            tracing::error!("Failed to send shutdown message to watcher thread: {:?}", e);
        }
        if let Some(thread) = self._thread.take()
            && let Err(e) = thread.join()
        {
            tracing::error!("Failed to join watcher thread: {:?}", e);
        }
    }
}

impl WatcherThreadInternal {
    fn run(self) {
        tracing::info!("Watcher thread started");
        while let Ok(message) = self.receiver.recv() {
            tracing::trace!("Watcher thread received message: {:?}", message);
            match message {
                WatcherMessage::ObjectChanged | WatcherMessage::ObjectSelected => {
                    let res = crate::EDIT_HANDLE.call_read_section(|read| {
                        let Some(selected_object) = read.get_focused_object()? else {
                            tracing::debug!("No focused object found");
                            crate::gui::update_object_effects_info(None);
                            return anyhow::Ok(());
                        };
                        let info =
                            crate::curve_io::read_object_effects_info(&read, selected_object)?;

                        tracing::debug!("Read object effects info: {:?}", info);
                        crate::gui::update_object_effects_info(Some(info));

                        anyhow::Ok(())
                    });
                    if let Err(e) = res {
                        tracing::warn!("Failed to read object effects info: {:?}", e);
                    }
                }
                WatcherMessage::Shutdown => break,
            }
        }
    }
}

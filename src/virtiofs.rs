use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::thread::{self, JoinHandle};

use fuse_backend_rs::api::server::Server;
use fuse_backend_rs::transport::{FsCacheReqHandler, Reader, VirtioFsWriter};
use thiserror::Error;
use tracing::{debug, error, warn};
use vhost::vhost_user::{Backend, Listener, message::*};
use vhost_user_backend::{
    VhostUserBackendMut, VhostUserDaemon, VringMutex, VringState, VringT,
};
use virtio_bindings::bindings::virtio_ring::{
    VIRTIO_RING_F_EVENT_IDX, VIRTIO_RING_F_INDIRECT_DESC,
};
use virtio_queue::{DescriptorChain, QueueOwnedT};
use vm_memory::{GuestAddressSpace, GuestMemoryAtomic, GuestMemoryLoadGuard, GuestMemoryMmap};
use vmm_sys_util::epoll::EventSet;
use vmm_sys_util::eventfd::EventFd;

use crate::filesystem::{Flocon, WinterFsHandler};

const VIRTIO_F_VERSION_1: u32 = 32;
const QUEUE_SIZE: usize = 1024;
const NUM_QUEUES: usize = 2;

// The guest queued an available buffer for the high priority queue.
const HIPRIO_QUEUE_EVENT: u16 = 0;
// The guest queued an available buffer for the request queue.
const REQ_QUEUE_EVENT: u16 = 1;

type VhostUserBackendResult<T> = std::io::Result<T>;

#[derive(Debug, Error)]
enum VirtiofsError {
    #[error("Failed to handle event, not an EPOLLIN event")]
    HandleEventNotEpollIn,
    #[error("Failed to handle event, unknown event: {0}")]
    HandleEventUnknownEvent(u16),
    #[error("Failed to iterate virtio queue")]
    IterateQueue,
    #[error("Invalid descriptor chain")]
    InvalidDescriptorChain(fuse_backend_rs::transport::Error),
    #[error("Failed to process virtio queue")]
    ProcessQueue(fuse_backend_rs::Error),
    #[error("Guest memory is not set")]
    QueueMemoryUnset,
    #[error("Failed to create new EventFd")]
    EventFdCreate(std::io::Error),
    #[error("Failed to start vhost-user daemon")]
    StartDaemon(vhost_user_backend::Error),
    #[error("Failed to create vhost-user listener")]
    CreateListener(std::io::Error),
    #[error("Failed to spawn daemon thread")]
    ThreadSpawn(std::io::Error),
}

impl From<VirtiofsError> for std::io::Error {
    fn from(e: VirtiofsError) -> Self {
        std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
    }
}

struct VhostUserFsBackend {
    event_idx: bool,
    kill_evt: EventFd,
    mem: Option<GuestMemoryAtomic<GuestMemoryMmap>>,
    server: Arc<Server<Arc<WinterFsHandler<Flocon>>>>,
    vu_req: Option<Backend>,
}

impl VhostUserFsBackend {
    fn process_queue(&mut self, vring_state: &mut MutexGuard<VringState>) -> std::io::Result<bool> {
        let guest_mem = self.mem.as_ref().ok_or(VirtiofsError::QueueMemoryUnset)?;
        let mut used_any = false;

        let avail_chains: Vec<DescriptorChain<GuestMemoryLoadGuard<GuestMemoryMmap>>> = vring_state
            .get_queue_mut()
            .iter(guest_mem.memory())
            .map_err(|_| VirtiofsError::IterateQueue)?
            .collect();

        for chain in avail_chains {
            used_any = true;

            let head_index = chain.head_index();
            let mem = chain.memory();

            let reader = Reader::from_descriptor_chain(mem, chain.clone())
                .map_err(VirtiofsError::InvalidDescriptorChain)?;
            let writer = VirtioFsWriter::new(mem, chain.clone())
                .map(|w| w.into())
                .map_err(VirtiofsError::InvalidDescriptorChain)?;

            // Backend doesn't implement FsCacheReqHandler, so we'll pass None instead
            let vu_req_handler: Option<&mut dyn FsCacheReqHandler> = None;

            self.server
                .handle_message(reader, writer, vu_req_handler, None)
                .map_err(VirtiofsError::ProcessQueue)?;

            if self.event_idx {
                if vring_state.add_used(head_index, 0).is_err() {
                    warn!("Couldn't return used descriptors to the ring");
                }

                match vring_state.needs_notification() {
                    Err(_) => {
                        warn!("Couldn't check if queue needs to be notified");
                        vring_state.signal_used_queue().unwrap();
                    }
                    Ok(needs_notification) => {
                        if needs_notification {
                            vring_state.signal_used_queue().unwrap();
                        }
                    }
                }
            } else {
                if vring_state.add_used(head_index, 0).is_err() {
                    warn!("Couldn't return used descriptors to the ring");
                }
                vring_state.signal_used_queue().unwrap();
            }
        }

        Ok(used_any)
    }
}

#[derive(Clone)]
struct VhostUserFsBackendHandler {
    backend: Arc<Mutex<VhostUserFsBackend>>,
}

impl VhostUserFsBackendHandler {
    fn new(server: Arc<Server<Arc<WinterFsHandler<Flocon>>>>) -> std::io::Result<Self> {
        let backend = VhostUserFsBackend {
            event_idx: false,
            kill_evt: EventFd::new(libc::EFD_NONBLOCK).map_err(VirtiofsError::EventFdCreate)?,
            mem: None,
            server,
            vu_req: None,
        };

        Ok(VhostUserFsBackendHandler {
            backend: Arc::new(Mutex::new(backend)),
        })
    }
}

impl VhostUserBackendMut for VhostUserFsBackendHandler {
    type Bitmap = ();
    type Vring = VringMutex;

    fn num_queues(&self) -> usize {
        NUM_QUEUES
    }

    fn max_queue_size(&self) -> usize {
        QUEUE_SIZE
    }

    fn features(&self) -> u64 {
        1 << VIRTIO_F_VERSION_1
            | 1 << VIRTIO_RING_F_INDIRECT_DESC
            | 1 << VIRTIO_RING_F_EVENT_IDX
            | VhostUserVirtioFeatures::PROTOCOL_FEATURES.bits()
    }

    fn protocol_features(&self) -> VhostUserProtocolFeatures {
        VhostUserProtocolFeatures::MQ | VhostUserProtocolFeatures::BACKEND_REQ
    }

    fn set_event_idx(&mut self, enabled: bool) {
        self.backend.lock().unwrap().event_idx = enabled;
    }

    fn update_memory(
        &mut self,
        mem: GuestMemoryAtomic<GuestMemoryMmap>,
    ) -> VhostUserBackendResult<()> {
        self.backend.lock().unwrap().mem = Some(mem);
        Ok(())
    }

    fn set_backend_req_fd(&mut self, vu_req: Backend) {
        self.backend.lock().unwrap().vu_req = Some(vu_req);
    }

    fn handle_event(
        &mut self,
        device_event: u16,
        evset: EventSet,
        vrings: &[VringMutex],
        _thread_id: usize,
    ) -> VhostUserBackendResult<()> {
        if evset != EventSet::IN {
            return Err(VirtiofsError::HandleEventNotEpollIn.into());
        }

        let mut vring_state = match device_event {
            HIPRIO_QUEUE_EVENT => {
                debug!("HIPRIO_QUEUE_EVENT");
                vrings[0].get_mut()
            }
            REQ_QUEUE_EVENT => {
                debug!("REQ_QUEUE_EVENT");
                vrings[1].get_mut()
            }
            _ => return Err(VirtiofsError::HandleEventUnknownEvent(device_event).into()),
        };

        let mut backend = self.backend.lock().unwrap();

        if backend.event_idx {
            loop {
                vring_state.disable_notification().unwrap();
                backend.process_queue(&mut vring_state)?;
                if !vring_state.enable_notification().unwrap() {
                    break;
                }
            }
        } else {
            backend.process_queue(&mut vring_state)?;
        }

        Ok(())
    }
}

/// Creates and starts the virtio-fs daemon in a new thread.
pub fn run_virtiofs_daemon(
    socket: PathBuf,
    server: Arc<Server<Arc<WinterFsHandler<Flocon>>>>,
) -> std::io::Result<JoinHandle<()>> {
    let handler = VhostUserFsBackendHandler::new(server)?;

    let mut daemon = VhostUserDaemon::new(
        String::from("flocon-virtiofs-backend"),
        Arc::new(RwLock::new(handler)),
        GuestMemoryAtomic::new(GuestMemoryMmap::new()),
    )
    .map_err(VirtiofsError::StartDaemon)?;

    let listener = Listener::new(socket, true).map_err(|e| VirtiofsError::CreateListener(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;

    let handle = thread::Builder::new()
        .name("virtiofs-daemon".to_string())
        .spawn(move || {
            if let Err(e) = daemon.start(listener) {
                error!("vhost-user-fs daemon failed: {}", e);
            }
        })
        .map_err(VirtiofsError::ThreadSpawn)?;

    Ok(handle)
}

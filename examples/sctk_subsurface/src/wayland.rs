use cctk::sctk::{
    self,
    reexports::{
        calloop_wayland_source::WaylandSource,
        client::{
            delegate_noop,
            globals::registry_queue_init,
            protocol::{wl_buffer::WlBuffer, wl_shm},
            Connection,
        },
    },
    registry::{ProvidesRegistryState, RegistryState},
    shm::{Shm, ShmHandler},
};
use futures_channel::mpsc;
use iced::{
    futures::{FutureExt, SinkExt},
    platform_specific::shell::subsurface_widget::{Shmbuf, SubsurfaceBuffer},
};
use rustix::{io::Errno, shm::ShmOFlags};
use std::{
    hash::{Hash, Hasher},
    os::{fd::OwnedFd, unix::io::AsRawFd},
    sync::Arc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

/// Wrapper around [`Connection`] that implements [`Hash`] via the poll fd.
#[derive(Clone)]
struct HashableConnection(Connection);

impl Hash for HashableConnection {
    fn hash<H: Hasher>(&self, state: &mut H) {
        use std::os::unix::io::AsFd;
        self.0.as_fd().as_raw_fd().hash(state);
    }
}

#[derive(Debug, Clone)]
pub enum Event {
    RedBuffer(SubsurfaceBuffer),
    GreenBuffer(SubsurfaceBuffer),
}

struct AppData {
    registry_state: RegistryState,
    shm_state: Shm,
}

impl ProvidesRegistryState for AppData {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    cctk::sctk::registry_handlers!();
}

impl ShmHandler for AppData {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm_state
    }
}

pub fn subscription(connection: &Connection) -> iced::Subscription<Event> {
    iced::Subscription::run_with(
        HashableConnection(connection.clone()),
        |conn| {
            let conn = conn.0.clone();
            async move { start(conn).await }.flatten_stream()
        },
    )
}

async fn start(conn: Connection) -> mpsc::Receiver<Event> {
    let (mut sender, receiver) = mpsc::channel(20);

    let (globals, event_queue) = registry_queue_init(&conn).unwrap();
    let qh = event_queue.handle();

    let mut app_data = AppData {
        registry_state: RegistryState::new(&globals),
        shm_state: Shm::bind(&globals, &qh).unwrap(),
    };

    let fd = create_memfile().unwrap();
    rustix::io::write(&fd, &[0, 255, 0, 255]).unwrap();

    let shmbuf = Shmbuf {
        fd,
        offset: 0,
        width: 1,
        height: 1,
        stride: 4,
        format: wl_shm::Format::Xrgb8888,
    };

    let buffer = SubsurfaceBuffer::new(Arc::new(shmbuf.into())).0;
    let _ = sender.send(Event::GreenBuffer(buffer)).await;

    let fd = create_memfile().unwrap();
    rustix::io::write(&fd, &[0, 0, 255, 255]).unwrap();

    let shmbuf = Shmbuf {
        fd,
        offset: 0,
        width: 1,
        height: 1,
        stride: 4,
        format: wl_shm::Format::Xrgb8888,
    };
    let buffer = SubsurfaceBuffer::new(Arc::new(shmbuf.into())).0;
    let _ = sender.send(Event::RedBuffer(buffer)).await;

    thread::spawn(move || {
        let mut event_loop = calloop::EventLoop::try_new().unwrap();
        WaylandSource::new(conn, event_queue)
            .insert(event_loop.handle())
            .unwrap();
        loop {
            event_loop.dispatch(None, &mut app_data).unwrap();
            std::thread::sleep(Duration::from_millis(500));
        }
    });

    receiver
}

fn create_memfile() -> rustix::io::Result<OwnedFd> {
    loop {
        let flags = ShmOFlags::CREATE | ShmOFlags::EXCL | ShmOFlags::RDWR;

        let time = SystemTime::now();
        let name = format!(
            "/iced-sctk-{}",
            time.duration_since(UNIX_EPOCH).unwrap().subsec_nanos()
        );

        match rustix::io::retry_on_intr(|| {
            rustix::shm::shm_open(&name, flags, 0600.into())
        }) {
            Ok(fd) => match rustix::shm::shm_unlink(&name) {
                Ok(_) => return Ok(fd),
                Err(errno) => {
                    return Err(errno.into());
                }
            },
            Err(Errno::EXIST) => {
                continue;
            }
            Err(err) => return Err(err.into()),
        }
    }
}

delegate_noop!(AppData: ignore WlBuffer);
sctk::delegate_registry!(AppData);
sctk::delegate_shm!(AppData);

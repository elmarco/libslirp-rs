use std::error::Error;
use std::os::unix::io::AsRawFd;
use std::os::unix::io::FromRawFd;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;

use libslirp;
use mio::{Events, Poll};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
#[structopt(name = "slirp", about = "slirp helper process")]
struct Opt {
    /// Activate debug mode
    #[structopt(long)]
    debug: bool,
    /// Unix datagram socket path
    #[structopt(name = "path", parse(from_os_str), long = "socket-path")]
    socket_path: Option<PathBuf>,
    /// Unix datagram socket file descriptor
    #[structopt(long)]
    fd: Option<i32>,

    #[structopt(flatten)]
    slirp: libslirp::Opt,
}

fn main() -> Result<(), Box<dyn Error>> {
    let opt = Opt::from_args();
    if opt.debug {
        dbg!(&opt);
    }
    let stream = match &opt {
        Opt { fd: Some(fd), .. } => unsafe { UnixDatagram::from_raw_fd(*fd) },
        Opt {
            socket_path: Some(path),
            ..
        } => UnixDatagram::bind(path)?,
        _ => panic!("Missing a socket argument"),
    };

    let poll = Poll::new()?;
    let mut slirp = libslirp::MioHandler::new(&opt.slirp, &poll, stream.as_raw_fd());

    let mut events = Events::with_capacity(1024);
    let mut duration = None;

    loop {
        if opt.debug {
            dbg!(duration);
        }

        poll.poll(&mut events, duration)?;
        duration = slirp.dispatch(&events)?;
    }
}

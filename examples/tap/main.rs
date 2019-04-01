use libslirp;
use mio::{Events, Poll};
use std::error::Error;
use std::os::unix::io::AsRawFd;
use std::process::Command;
use std::rc::Rc;
use structopt::StructOpt;
use tun_tap::{Iface, Mode};

fn cmd(cmd: &str, args: &[&str]) {
    let ecode = Command::new(cmd)
        .args(args)
        .spawn()
        .unwrap()
        .wait()
        .unwrap();
    assert!(ecode.success(), "Failed to exec {}", cmd);
}

fn main() -> Result<(), Box<dyn Error>> {
    let opt = libslirp::Opt::from_args();
    let iface = Rc::new(Iface::without_packet_info("testtap%d", Mode::Tap)?);
    let mut addr = opt.ipv4.net.to_string();
    addr.push_str("/24");
    cmd("ip", &["addr", "add", "dev", iface.name(), addr.as_str()]);
    cmd("ip", &["link", "set", "up", "dev", iface.name()]);

    let poll = Poll::new()?;
    let mut slirp = libslirp::MioHandler::new(&opt, &poll, iface.as_raw_fd());

    let mut events = Events::with_capacity(1024);
    let mut duration = None;

    loop {
        poll.poll(&mut events, duration)?;
        duration = slirp.dispatch(&events)?;
    }
}

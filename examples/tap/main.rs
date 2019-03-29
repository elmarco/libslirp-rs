use libslirp;
use std::io;
use std::os::unix::io::RawFd;
use std::process::Command;
use std::rc::Rc;
use std::time::Instant;
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

struct App {
    start: Instant,
    iface: Rc<Iface>,
}

impl libslirp::Handler for App {
    type Timer = usize;

    fn clock_get_ns(&mut self) -> i64 {
        const NANOS_PER_SEC: u64 = 1_000_000_000;
        let d = self.start.elapsed();
        (d.as_secs() * NANOS_PER_SEC + d.subsec_nanos() as u64) as i64
    }

    fn timer_new(&mut self, _func: Box<dyn FnMut()>) -> Box<Self::Timer> {
        Box::new(0)
    }

    fn timer_mod(&mut self, _timer: &mut Box<Self::Timer>, _expire_time: i64) {}

    fn timer_free(&mut self, timer: Box<Self::Timer>) {
        drop(timer);
    }

    fn send_packet(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.iface.send(buf)
    }

    fn guest_error(&mut self, msg: &str) {
        eprintln!("guest error: {}", msg);
    }

    fn register_poll_fd(&mut self, fd: RawFd) {
        println!("register_poll_fd: fd={:?}", fd);
    }

    fn unregister_poll_fd(&mut self, fd: RawFd) {
        println!("unregister_poll_fd: fd={:?}", fd);
    }

    fn notify(&mut self) {
        println!("notify");
    }
}

fn main() -> io::Result<()> {
    let opt = libslirp::Opt::from_args();
    let iface = Rc::new(Iface::without_packet_info("testtap%d", Mode::Tap)?);
    let app = App {
        start: Instant::now(),
        iface: iface.clone(),
    };
    let mut ctxt = libslirp::Context::new_with_opt(&opt, app);
    let mut addr = opt.ipv4.net.to_string();
    addr.push_str("/24");
    cmd("ip", &["addr", "add", "dev", iface.name(), addr.as_str()]);
    cmd("ip", &["link", "set", "up", "dev", iface.name()]);

    loop {
        let mut buffer = vec![0; 1500];
        let size = iface.recv(&mut buffer)?;
        ctxt.input(&buffer[..size]);
    }
}

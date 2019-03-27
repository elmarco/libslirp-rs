use std::cell::RefCell;
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use libslirp;
use mio::unix::{EventedFd, UnixReady};
use mio::*;
use mio_extras::timer::Timer as MioTimer;
use slab::Slab;
use std::os::unix::io::AsRawFd;
use structopt::StructOpt;

struct MyTimer {
    func: Rc<RefCell<Box<dyn FnMut()>>>,
    timer: MioTimer<()>,
}

impl<'a> libslirp::Handler for App<'a> {
    type Timer = usize;

    fn clock_get_ns(&mut self) -> i64 {
        const NANOS_PER_SEC: u64 = 1_000_000_000;
        let d = self.start.elapsed();
        (d.as_secs() * NANOS_PER_SEC + d.subsec_nanos() as u64) as i64
    }

    fn timer_new(&mut self, func: Box<dyn FnMut()>) -> Box<Self::Timer> {
        let timer = MioTimer::default();
        let tok = self.tokens.insert(MyToken::Timer(MyTimer {
            func: Rc::new(RefCell::new(func)),
            timer,
        }));
        let timer = match &self.tokens[tok] {
            MyToken::Timer(MyTimer { timer: t, .. }) => t,
            _ => panic!(),
        };

        self.poll
            .register(timer, Token(tok), Ready::readable(), PollOpt::edge())
            .unwrap();

        Box::new(tok)
    }

    fn timer_mod(&mut self, timer: &mut Box<Self::Timer>, expire_time: i64) {
        let when = Duration::from_nanos(expire_time as u64);
        let timer = match &mut self.tokens[**timer] {
            MyToken::Timer(MyTimer { timer: t, .. }) => t,
            _ => panic!(),
        };
        if self.opt.debug {
            dbg!(("timer_mod", when));
        }
        timer.set_timeout(when, ());
    }

    fn timer_free(&mut self, timer: Box<Self::Timer>) {
        let t = match &self.tokens[*timer] {
            MyToken::Timer(MyTimer { timer: t, .. }) => t,
            _ => panic!(),
        };

        self.poll.deregister(t).unwrap();

        self.tokens.remove(*timer);
        drop(timer); // for clarity
    }

    fn send_packet(&mut self, buf: &[u8]) -> isize {
        self.stream.send(buf).unwrap() as isize
    }

    fn guest_error(&mut self, msg: &str) {
        eprintln!("guest error: {}", msg);
    }

    fn register_poll_fd(&mut self, fd: RawFd) {
        if self.opt.debug {
            println!("register_poll_fd: fd={:?}", fd);
        }
    }

    fn unregister_poll_fd(&mut self, fd: RawFd) {
        if self.opt.debug {
            println!("unregister_poll_fd: fd={:?}", fd);
        }
    }

    fn notify(&mut self) {
        if self.opt.debug {
            println!("notify");
        }
    }
}

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

#[derive(Debug)]
struct MyFd {
    fd: RawFd,
    events: libslirp::PollEvents,
    revents: Option<libslirp::PollEvents>,
}

impl MyFd {
    fn new(fd: RawFd, events: libslirp::PollEvents) -> Self {
        Self {
            events,
            fd,
            revents: None,
        }
    }
}

struct App<'a> {
    opt: &'a Opt,
    start: Instant,
    stream: UnixDatagram,
    ctxt: Option<libslirp::Context<Rc<RefCell<Self>>>>,
    poll: Poll,
    tokens: Slab<MyToken>,
}

enum MyToken {
    Fd(MyFd),
    Timer(MyTimer),
}

impl<'a> App<'a> {
    fn new(opt: &'a Opt) -> Rc<RefCell<Self>> {
        let stream = match &opt {
            Opt { fd: Some(fd), .. } => unsafe { UnixDatagram::from_raw_fd(*fd) },
            Opt {
                socket_path: Some(path),
                ..
            } => UnixDatagram::bind(path).unwrap(),
            _ => panic!("Missing a socket argument"),
        };

        let app = Rc::new(RefCell::new(Self {
            opt,
            stream,
            start: Instant::now(),
            ctxt: None,
            poll: Poll::new().unwrap(),
            tokens: Slab::with_capacity(1024),
        }));

        let ctxt = libslirp::Context::new_with_opt(&opt.slirp, app.clone());
        app.borrow_mut().ctxt = Some(ctxt);

        app
    }
}

fn to_mio_ready(events: libslirp::PollEvents) -> mio::Ready {
    let mut ready = UnixReady::from(Ready::empty());

    if events.has_in() {
        ready.insert(Ready::readable());
    }
    if events.has_out() {
        ready.insert(Ready::writable());
    }
    if events.has_hup() {
        ready.insert(UnixReady::hup());
    }
    if events.has_err() {
        ready.insert(UnixReady::error());
    }
    if events.has_pri() {
        ready.insert(UnixReady::priority());
    }

    Ready::from(ready)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libslirp::PollEvents;

    #[test]
    fn to_mio_ready_test() {
        assert_eq!(to_mio_ready(PollEvents::empty()), Ready::empty());
        assert_eq!(to_mio_ready(PollEvents::poll_in()), Ready::readable());
        assert_eq!(to_mio_ready(PollEvents::poll_out()), Ready::writable());
        assert_eq!(
            to_mio_ready(PollEvents::poll_err()),
            Ready::from(UnixReady::error())
        );
        assert_eq!(
            to_mio_ready(PollEvents::poll_pri()),
            Ready::from(UnixReady::priority())
        );
        assert_eq!(
            to_mio_ready(PollEvents::poll_hup()),
            Ready::from(UnixReady::hup())
        );
        let ev = PollEvents::poll_in() | PollEvents::poll_pri();
        let ev = to_mio_ready(ev);
        assert!(ev.is_readable());
        // bug, see https://github.com/carllerche/mio/pull/897
        assert!(!ev.is_writable());
    }
}

fn from_mio_ready(ready: mio::Ready) -> libslirp::PollEvents {
    use libslirp::PollEvents;

    let mut events = PollEvents::empty();
    let ready = UnixReady::from(ready);

    if ready.is_readable() {
        events |= PollEvents::poll_in();
    }
    if ready.is_writable() {
        events |= PollEvents::poll_out();
    }
    if ready.is_hup() {
        events |= PollEvents::poll_hup();
    }
    if ready.is_error() {
        events |= PollEvents::poll_err();
    }
    if ready.is_priority() {
        events |= PollEvents::poll_pri();
    }

    events
}

fn main() {
    let opt = Opt::from_args();
    if opt.debug {
        dbg!(&opt);
    }
    let app = App::new(&opt);

    // {
    //     use std::io::prelude::*;
    //     use std::io::Cursor;

    //     // test state saving/restore
    //     let mut state = vec![];
    //     ctxt.state_save(|buf| state.write(buf).unwrap() as isize);
    //     let mut state = Cursor::new(state);
    //     ctxt.state_load(state_version(), |mut buf| {
    //         let ret = state.read(&mut buf).unwrap() as isize;
    //         //println!("read {} {} {:?}", ret, buf.len(), buf);
    //         ret
    //     });
    // }

    const SOCKET: Token = Token(10_000_000);
    {
        let app = app.borrow_mut();
        let fd = app.stream.as_raw_fd();
        app.poll
            .register(&EventedFd(&fd), SOCKET, Ready::readable(), PollOpt::level())
            .unwrap();
    }

    let mut events = Events::with_capacity(1024);
    let mut duration = None;
    loop {
        if opt.debug {
            dbg!(duration);
        }
        app.borrow().poll.poll(&mut events, duration).unwrap();

        for event in &events {
            let mut timer_func = None;

            match event.token() {
                SOCKET => {
                    const NET_BUFSIZE: usize = 4096 + 65536; // defined by Qemu
                    let mut buffer = [0; NET_BUFSIZE];

                    let len = app.borrow_mut().stream.recv(&mut buffer[..]).unwrap();
                    let mut ctxt = app.borrow_mut().ctxt.take().unwrap();
                    ctxt.input(&buffer[..len]);
                    app.borrow_mut().ctxt.replace(ctxt);
                }
                i => {
                    let events = from_mio_ready(event.readiness());
                    let mut app = app.borrow_mut();
                    let token = &mut app.tokens[i.0];
                    match token {
                        MyToken::Fd(fd) => {
                            // libslirp doesn't like getting more events...
                            fd.revents = Some(events & fd.events);
                        }
                        MyToken::Timer(MyTimer { func, .. }) => {
                            timer_func = Some(func.clone());
                        }
                    }
                }
            }

            if let Some(func) = timer_func {
                // really? I must be doing something wrong
                let func = &mut **func.borrow_mut();
                func();
            }
        }

        let mut ctxt = app.borrow_mut().ctxt.take().unwrap();

        ctxt.pollfds_poll(false, |idx| {
            let token = &mut app.borrow_mut().tokens[idx as usize];
            if let MyToken::Fd(fd) = token {
                fd.revents.take().unwrap_or(libslirp::PollEvents::empty())
            } else {
                panic!();
            }
        });

        let mut to_remove = vec![];
        for (idx, token) in app.borrow().tokens.iter() {
            if let MyToken::Fd(fd) = token {
                let ev = EventedFd(&fd.fd);
                app.borrow().poll.deregister(&ev).unwrap();
                to_remove.push(idx);
            }
        }
        for idx in to_remove.iter() {
            app.borrow_mut().tokens.remove(*idx);
        }

        let mut timeout = 0;
        ctxt.pollfds_fill(&mut timeout, |fd, events| {
            let mut app = app.borrow_mut();
            let ready = to_mio_ready(events);
            let tok = app.tokens.insert(MyToken::Fd(MyFd::new(fd, events)));
            let ev = EventedFd(&fd);

            app.poll
                .register(&ev, Token(tok), ready, PollOpt::level())
                .unwrap();

            tok as i32
        });
        duration = if timeout == 0 {
            None
        } else {
            Some(Duration::from_millis(timeout as u64))
        };

        app.borrow_mut().ctxt.replace(ctxt);
    }
}

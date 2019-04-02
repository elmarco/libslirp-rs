use crate::context::{Context, Handler, PollEvents};
use crate::opt::Opt;

use mio::unix::{EventedFd, UnixReady};
use mio::*;
use mio_extras::timer::Timer as MioTimer;
use slab::Slab;
use std::cell::RefCell;
use std::fmt;
use std::fs::File;
use std::io;
use std::io::prelude::*;
use std::os::unix::io::{FromRawFd, RawFd};
use std::rc::Rc;
use std::time::{Duration, Instant};

struct MyTimer {
    func: Rc<RefCell<Box<dyn FnMut()>>>,
    timer: MioTimer<()>,
}

impl fmt::Debug for MyTimer {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "MyTimer {{}}")
    }
}

#[derive(Debug)]
struct MyFd {
    fd: RawFd,
    events: PollEvents,
    revents: Option<PollEvents>,
}

impl MyFd {
    fn new(fd: RawFd, events: PollEvents) -> Self {
        Self {
            events,
            fd,
            revents: None,
        }
    }
}

#[derive(Debug)]
enum MyToken {
    Fd(MyFd),
    Timer(MyTimer),
}

struct Inner<'a> {
    start: Instant,
    stream: File,
    poll: &'a Poll,
    tokens: Slab<MyToken>,
}

pub struct MioHandler<'a> {
    inner: Rc<RefCell<Inner<'a>>>,
    ctxt: Context<Rc<RefCell<Inner<'a>>>>,
}

impl<'a> Handler for Inner<'a> {
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

    fn send_packet(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }

    fn guest_error(&mut self, msg: &str) {
        eprintln!("guest error: {}", msg);
    }

    fn register_poll_fd(&mut self, _fd: RawFd) {}

    fn unregister_poll_fd(&mut self, _fd: RawFd) {}

    fn notify(&mut self) {}
}

fn to_mio_ready(events: PollEvents) -> mio::Ready {
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
    use PollEvents;

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

fn from_mio_ready(ready: mio::Ready) -> PollEvents {
    use PollEvents;

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

const SOCKET: Token = Token(10_000_000);

impl<'a> MioHandler<'a> {
    pub fn new(opt: &Opt, poll: &'a Poll, fd: RawFd) -> Self {
        poll.register(&EventedFd(&fd), SOCKET, Ready::readable(), PollOpt::level())
            .unwrap();

        let inner = Rc::new(RefCell::new(Inner {
            start: Instant::now(),
            poll,
            stream: unsafe { File::from_raw_fd(fd) },
            tokens: Slab::with_capacity(1024),
        }));

        Self {
            inner: inner.clone(),
            ctxt: Context::new_with_opt(opt, inner.clone()),
        }
    }

    pub fn dispatch(&mut self, events: &Events) -> io::Result<Option<Duration>> {
        let inner = self.inner.clone();

        for (_, token) in inner.borrow().tokens.iter() {
            if let MyToken::Fd(fd) = token {
                let ev = EventedFd(&fd.fd);
                inner.borrow().poll.deregister(&ev)?;
            }
        }

        for event in events {
            match event.token() {
                SOCKET => {
                    const NET_BUFSIZE: usize = 4096 + 65536; // defined by Emu
                    let mut buffer = [0; NET_BUFSIZE];

                    let len = self
                        .inner
                        .borrow_mut()
                        .stream
                        .read(&mut buffer[..])
                        .unwrap();
                    self.ctxt.input(&buffer[..len]);
                }
                i => {
                    let events = from_mio_ready(event.readiness());
                    let mut inner = inner.borrow_mut();
                    let token = &mut inner.tokens[i.0];

                    match token {
                        MyToken::Fd(fd) => {
                            // libslirp doesn't like getting more events...
                            fd.revents = Some(events & fd.events);
                        }
                        MyToken::Timer(MyTimer { func, .. }) => {
                            let func = func.clone();
                            drop(inner);
                            let func = &mut **func.borrow_mut();
                            func();
                        }
                    }
                }
            }
        }

        self.ctxt.pollfds_poll(false, |idx| {
            let token = &mut inner.borrow_mut().tokens[idx as usize];
            if let MyToken::Fd(fd) = token {
                fd.revents.take().unwrap_or(PollEvents::empty())
            } else {
                panic!();
            }
        });

        inner
            .borrow_mut()
            .tokens
            .retain(|_, v| if let MyToken::Fd(_) = v { false } else { true });

        let mut timeout = 0;
        self.ctxt.pollfds_fill(&mut timeout, |fd, events| {
            let ready = to_mio_ready(events);
            let tok = inner
                .borrow_mut()
                .tokens
                .insert(MyToken::Fd(MyFd::new(fd, events)));
            let ev = EventedFd(&fd);

            inner
                .borrow()
                .poll
                .register(&ev, Token(tok), ready, PollOpt::level())
                .unwrap();

            tok as i32
        });

        let duration = if timeout == 0 {
            None
        } else {
            Some(Duration::from_millis(timeout as u64))
        };

        Ok(duration)
    }
}

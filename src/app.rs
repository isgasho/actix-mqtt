use std::rc::Rc;

use actix_router::{Router, RouterBuilder};
use actix_service::boxed::{self, BoxedNewService, BoxedService};
use actix_service::{IntoConfigurableNewService, IntoNewService, NewService, Service};
use futures::future::{err, join_all, Either, FutureResult, JoinAll};
use futures::{Async, Future, Poll};

use crate::publish::Publish;

type Handler<S, E> = BoxedNewService<S, Publish<S>, (), E, E>;
type HandlerService<S, E> = BoxedService<Publish<S>, (), E>;

pub struct App<S, E> {
    router: RouterBuilder<usize>,
    handlers: Vec<Handler<S, E>>,
    not_found: Rc<Fn(Publish<S>) -> E>,
}

impl<S, E> App<S, E>
where
    S: 'static,
    E: 'static,
{
    /// Create mqtt application and provide default topic handler.
    pub fn new<F>(not_found: F) -> Self
    where
        F: Fn(Publish<S>) -> E + 'static,
    {
        App {
            router: Router::build(),
            handlers: Vec::new(),
            not_found: Rc::new(not_found),
        }
    }

    pub fn resource<F, U: 'static>(mut self, address: &str, service: F) -> Self
    where
        F: IntoNewService<U, S>,
        U: NewService<S, Request = Publish<S>, Response = ()>,
        E: From<U::Error> + From<U::InitError>,
    {
        self.router.path(address, self.handlers.len());
        self.handlers.push(boxed::new_service(
            service
                .into_new_service()
                .map_err(|e| e.into())
                .map_init_err(|e| e.into()),
        ));
        self
    }
}

impl<S, E> IntoConfigurableNewService<AppFactory<S, E>, S> for App<S, E>
where
    S: 'static,
    E: 'static,
{
    fn into_new_service(self) -> AppFactory<S, E> {
        AppFactory {
            router: Rc::new(self.router.finish()),
            handlers: self.handlers,
            not_found: self.not_found,
        }
    }
}

pub struct AppFactory<S, E> {
    router: Rc<Router<usize>>,
    handlers: Vec<Handler<S, E>>,
    not_found: Rc<Fn(Publish<S>) -> E>,
}

impl<S, E> NewService<S> for AppFactory<S, E>
where
    S: 'static,
    E: 'static,
{
    type Request = Publish<S>;
    type Response = ();
    type Error = E;
    type InitError = E;
    type Service = AppService<S, E>;
    type Future = AppFactoryFut<S, E>;

    fn new_service(&self, session: &S) -> Self::Future {
        let fut: Vec<_> = self
            .handlers
            .iter()
            .map(|h| h.new_service(session))
            .collect();

        AppFactoryFut {
            router: self.router.clone(),
            handlers: join_all(fut),
            not_found: self.not_found.clone(),
        }
    }
}

pub struct AppFactoryFut<S, E> {
    router: Rc<Router<usize>>,
    handlers: JoinAll<Vec<Box<Future<Item = HandlerService<S, E>, Error = E>>>>,
    not_found: Rc<Fn(Publish<S>) -> E>,
}

impl<S, E> Future for AppFactoryFut<S, E> {
    type Item = AppService<S, E>;
    type Error = E;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let handlers = futures::try_ready!(self.handlers.poll());
        Ok(Async::Ready(AppService {
            handlers,
            router: self.router.clone(),
            not_found: self.not_found.clone(),
        }))
    }
}

pub struct AppService<S, E> {
    router: Rc<Router<usize>>,
    handlers: Vec<BoxedService<Publish<S>, (), E>>,
    not_found: Rc<Fn(Publish<S>) -> E>,
}

impl<S, E> Service for AppService<S, E>
where
    S: 'static,
    E: 'static,
{
    type Request = Publish<S>;
    type Response = ();
    type Error = E;
    type Future = Either<
        FutureResult<Self::Response, Self::Error>,
        Box<Future<Item = Self::Response, Error = Self::Error>>,
    >;

    fn poll_ready(&mut self) -> Poll<(), Self::Error> {
        let mut not_ready = false;
        for hnd in &mut self.handlers {
            if let Async::NotReady = hnd.poll_ready()? {
                not_ready = true;
            }
        }

        if not_ready {
            Ok(Async::NotReady)
        } else {
            Ok(Async::Ready(()))
        }
    }

    fn call(&mut self, mut req: Publish<S>) -> Self::Future {
        if let Some((idx, _info)) = self.router.recognize(req.path_mut()) {
            self.handlers[*idx].call(req)
        } else {
            Either::A(err((*self.not_found.as_ref())(req)))
        }
    }
}

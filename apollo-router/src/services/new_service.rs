use tower::Service;

pub trait NewService<Request> {
    type Service: Service<Request>;

    fn new_service(&self) -> Self::Service;
}

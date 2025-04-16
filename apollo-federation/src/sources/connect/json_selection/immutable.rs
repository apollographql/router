use std::clone::Clone;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub(crate) struct InputPath<T: Clone> {
    path: Path<T>,
}

type Path<T> = Option<Rc<AppendPath<T>>>;

#[derive(Debug, Clone)]
struct AppendPath<T: Clone> {
    prefix: Path<T>,
    last: T,
}

impl<T: Clone> InputPath<T> {
    pub(crate) fn empty() -> InputPath<T> {
        InputPath { path: None }
    }

    pub(crate) fn append(&self, last: T) -> Self {
        Self {
            path: Some(Rc::new(AppendPath {
                prefix: self.path.clone(),
                last,
            })),
        }
    }

    pub(crate) fn to_vec(&self) -> Vec<T> {
        // This method needs to be iterative rather than recursive, to be
        // consistent with the paranoia of the drop method.
        let mut vec = Vec::new();
        let mut path = self.path.as_deref();
        while let Some(p) = path {
            vec.push(p.last.clone());
            path = p.prefix.as_deref();
        }
        vec.reverse();
        vec
    }
}

impl<T: Clone> Drop for InputPath<T> {
    fn drop(&mut self) {
        let mut path = self.path.take();
        while let Some(rc) = path {
            if let Ok(mut p) = Rc::try_unwrap(rc) {
                path = p.prefix.take();
            } else {
                break;
            }
        }
    }
}

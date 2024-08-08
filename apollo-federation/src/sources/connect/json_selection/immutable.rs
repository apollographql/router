use std::clone::Clone;
use std::rc::Rc;

#[derive(Clone)]
pub enum InputPath<T: Clone> {
    Empty,
    Tail(Rc<InputPath<T>>, T),
}

impl<T: Clone> InputPath<T> {
    pub fn append(&self, last: T) -> InputPath<T> {
        InputPath::Tail(Rc::new(self.clone()), last)
    }

    pub fn to_vec(&self) -> Vec<T> {
        match self {
            InputPath::Empty => vec![],
            InputPath::Tail(prefix, last) => {
                let mut prefix = prefix.to_vec();
                prefix.push(last.clone());
                prefix
            }
        }
    }
}

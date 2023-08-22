use tracing_core::field::Value;

pub(crate) trait TracingUtils {
    fn or_empty(&self) -> &dyn Value;
}

impl TracingUtils for bool {
    fn or_empty(&self) -> &dyn Value {
        if *self {
            self as &dyn Value
        } else {
            &::tracing::field::Empty
        }
    }
}

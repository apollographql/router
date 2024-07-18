use std::fmt;
use std::fmt::Display;

pub(crate) struct State<'fmt, 'fmt2> {
    indent_level: usize,
    output: &'fmt mut fmt::Formatter<'fmt2>,
}

impl<'a, 'b> State<'a, 'b> {
    pub(crate) fn new(output: &'a mut fmt::Formatter<'b>) -> State<'a, 'b> {
        Self {
            indent_level: 0,
            output,
        }
    }

    pub(crate) fn indent_level(&self) -> usize {
        self.indent_level
    }

    pub(crate) fn write<T: fmt::Display>(&mut self, value: T) -> fmt::Result {
        write!(self.output, "{}", value)
    }

    pub(crate) fn write_fmt(&mut self, args: fmt::Arguments<'_>) -> fmt::Result {
        self.output.write_fmt(args)
    }

    pub(crate) fn new_line(&mut self) -> fmt::Result {
        self.write("\n")?;
        for _ in 0..self.indent_level {
            self.write("  ")?
        }
        Ok(())
    }

    pub(crate) fn indent_no_new_line(&mut self) {
        self.indent_level += 1;
    }

    pub(crate) fn indent(&mut self) -> fmt::Result {
        self.indent_no_new_line();
        self.new_line()
    }

    pub(crate) fn dedent(&mut self) -> fmt::Result {
        self.indent_level -= 1;
        self.new_line()
    }
}

pub(crate) fn write_indented_lines<T>(
    state: &mut State<'_, '_>,
    values: &[T],
    mut write_line: impl FnMut(&mut State<'_, '_>, &T) -> fmt::Result,
) -> fmt::Result {
    if !values.is_empty() {
        state.indent_no_new_line();
        for value in values {
            state.new_line()?;
            write_line(state, value)?;
        }
        state.dedent()?;
    }
    Ok(())
}

pub(crate) struct DisplaySlice<'a, T>(pub(crate) &'a [T]);

impl<'a, T: Display> Display for DisplaySlice<'a, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[")?;
        let mut iter = self.0.iter();
        if let Some(item) = iter.next() {
            write!(f, "{item}")?;
        }
        iter.try_for_each(|item| write!(f, ", {item}"))?;
        write!(f, "]")
    }
}

pub(crate) struct DisplayOption<T>(pub(crate) Option<T>);

impl<T> DisplayOption<T> {
    pub(crate) fn new(inner: &Option<T>) -> DisplayOption<&T> {
        DisplayOption(inner.as_ref())
    }
}

impl<T: Display> Display for DisplayOption<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Some(item) => write!(f, "Some({item})"),
            None => write!(f, "None"),
        }
    }
}

use std::fmt;

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

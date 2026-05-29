use std::fmt::Formatter;

pub(super) struct DelimitedFormatter<'a, 'b> {
    formatter: &'a mut Formatter<'b>,
    delimiter: &'a str,
    wrote_prev: bool,
}

// Defaults to comma delimited
impl<'a, 'b> From<&'a mut Formatter<'b>> for DelimitedFormatter<'a, 'b> {
    fn from(formatter: &'a mut Formatter<'b>) -> Self {
        Self {
            formatter,
            delimiter: ",",
            wrote_prev: false,
        }
    }
}

impl<'a, 'b> DelimitedFormatter<'a, 'b> {
    pub(super) fn write_fmt(&mut self, fmt: std::fmt::Arguments<'_>) -> std::fmt::Result {
        if self.wrote_prev {
            self.formatter.write_str(self.delimiter)?;
        }

        self.formatter.write_fmt(fmt)?;
        self.wrote_prev = true;

        Ok(())
    }
}

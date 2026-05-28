use std::fmt::Formatter;

pub(super) struct DelimitedFormatter<'a> {
    formatter: &'a mut Formatter<'_>,
    delimiter: &'a str,
    wrote_prev: bool,
}

// Defaults to comma delimited
impl<'a> From<&'a mut Formatter<'_>> for DelimitedFormatter<'a> {
    fn from(formatter: &'a mut Formatter<'_>) -> Self {
        Self {
            formatter,
            delimiter: ",",
            wrote_prev: false,
        }
    }
}

impl<'a> DelimitedFormatter<'a> {
    fn write_fmt(&mut self, fmt: std::fmt::Arguments<'_>) -> std::fmt::Result {
        if self.wrote_prev {
            self.formatter.write_str(self.delimiter)?;
        }

        self.formatter.write_fmt(fmt)?;
        self.wrote_prev = true;

        Ok(())
    }
}

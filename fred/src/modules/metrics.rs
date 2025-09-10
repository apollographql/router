#![allow(unused_variables)]
#![allow(dead_code)]

use std::cmp;

/// Stats describing a distribution of samples.
///
/// Time units are in milliseconds, data size units are in bytes.
#[derive(Clone, Debug)]
pub struct Stats {
  pub min:     i64,
  pub max:     i64,
  pub avg:     f64,
  pub stddev:  f64,
  pub samples: u64,
  pub sum:     i64,
}

/// Struct for tracking moving stats about network latency or request/response sizes.
///
/// Time units are in milliseconds, data size units are in bytes.
pub struct MovingStats {
  pub min:      i64,
  pub max:      i64,
  pub avg:      f64,
  pub variance: f64,
  pub samples:  u64,
  pub sum:      i64,
  old_avg:      f64,
  s:            f64,
  old_s:        f64,
}

impl Default for MovingStats {
  fn default() -> Self {
    MovingStats {
      min:      0,
      max:      0,
      avg:      0.0,
      sum:      0,
      variance: 0.0,
      samples:  0,
      s:        0.0,
      old_s:    0.0,
      old_avg:  0.0,
    }
  }
}

impl MovingStats {
  pub fn sample(&mut self, value: i64) {
    self.samples += 1;
    let num_samples = self.samples as f64;
    let value_f = value as f64;
    self.sum += value;

    if self.samples == 1 {
      self.avg = value_f;
      self.variance = 0.0;
      self.old_avg = value_f;
      self.old_s = 0.0;
      self.min = value;
      self.max = value;
    } else {
      self.avg = self.old_avg + (value_f - self.old_avg) / num_samples;
      self.s = self.old_s + (value_f - self.old_avg) * (value_f - self.avg);

      self.old_avg = self.avg;
      self.old_s = self.s;
      self.variance = self.s / (num_samples - 1.0);

      self.min = cmp::min(self.min, value);
      self.max = cmp::max(self.max, value);
    }
  }

  pub fn reset(&mut self) {
    self.min = 0;
    self.max = 0;
    self.avg = 0.0;
    self.variance = 0.0;
    self.samples = 0;
    self.sum = 0;
    self.s = 0.0;
    self.old_s = 0.0;
    self.old_avg = 0.0;
  }

  pub fn read_metrics(&self) -> Stats {
    self.into()
  }

  pub fn take_metrics(&mut self) -> Stats {
    let metrics = self.read_metrics();
    self.reset();
    metrics
  }
}

impl<'a> From<&'a MovingStats> for Stats {
  fn from(stats: &'a MovingStats) -> Stats {
    Stats {
      avg:     stats.avg,
      stddev:  stats.variance.sqrt(),
      min:     stats.min,
      max:     stats.max,
      samples: stats.samples,
      sum:     stats.sum,
    }
  }
}

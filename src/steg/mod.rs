static DEFAULT_PADDING: usize = 0;

pub struct Hic {
  on_pubkey: bool,
  pubkey: InnerHic,
  payload: InnerHic,
}

impl Hic {
  pub fn new(pubkey: InnerHic, payload: InnerHic) -> Self {
    Self { on_pubkey: true, pubkey, payload }
  }

  pub fn just_data(
    data: Vec<u8>, steps_to_skip: Option<usize>, offset: Option<usize>,
  ) -> Self {
    Self {
      on_pubkey: false,
      pubkey: InnerHic::new(vec![], None, None),
      payload: InnerHic::new(data, steps_to_skip, offset),
    }
  }

  pub fn start_pre_encoding(&mut self) {
    if self.pubkey.is_done() && self.on_pubkey {
      self.on_pubkey = false;
      println!("Switching to data!");
    }

    if self.on_pubkey {
      self.pubkey.start_pre_encoding();
    } else {
      self.payload.start_pre_encoding();
    }
  }

  pub fn stop_pre_encoding(&mut self) {
    if self.on_pubkey {
      self.pubkey.stop_pre_encoding();
    } else {
      self.payload.stop_pre_encoding();
    }
  }

  pub fn start_final_encoding(&mut self) {
    if self.on_pubkey {
      self.pubkey.start_final_encoding();
    } else {
      self.payload.start_final_encoding();
    }
  }

  pub fn stop_final_encoding(&mut self) {
    if self.on_pubkey {
      self.pubkey.stop_final_encoding();
    } else {
      self.payload.stop_final_encoding();
    }
  }

  pub fn is_enabled(&self) -> bool {
    if self.on_pubkey {
      self.pubkey.is_enabled()
    } else {
      self.payload.is_enabled()
    }
  }

  pub fn is_done(&self) -> bool {
    if self.on_pubkey {
      self.pubkey.is_done()
    } else {
      self.payload.is_done()
    }
  }

  pub fn inject_in_angle(&mut self, angle: u32) -> u32 {
    if self.on_pubkey {
      self.pubkey.inject_in_angle(angle)
    } else {
      self.payload.inject_in_angle(angle)
    }
  }
}

pub struct InnerHic {
  pub data: Vec<u8>,

  pre_encoding: bool,
  booked_byte_index: usize,
  booked_bit_index: usize,
  booked_skipped_steps: usize,
  booked_offset: usize,

  final_encoding: bool,
  current_byte_index: usize,
  current_bit_index: usize,

  current_padding: usize,
  padding: usize,

  current_offset: usize,
  offset: usize,
}

impl InnerHic {
  pub fn new(
    data: Vec<u8>, steps_to_skip: Option<usize>, offset: Option<usize>,
  ) -> Self {
    InnerHic {
      data,

      pre_encoding: false,
      booked_byte_index: 0,
      booked_bit_index: 0,
      booked_skipped_steps: 0,
      booked_offset: 0,

      final_encoding: false,

      current_byte_index: 0,
      current_bit_index: 0,

      current_padding: 0,
      padding: steps_to_skip.unwrap_or(DEFAULT_PADDING),

      current_offset: 0,
      offset: offset.unwrap_or(0),
    }
  }

  pub fn start_pre_encoding(&mut self) {
    self.booked_byte_index = self.current_byte_index;
    self.booked_bit_index = self.current_bit_index;

    self.booked_skipped_steps = self.current_padding;
    self.booked_offset = self.offset;

    self.pre_encoding = true;
  }

  pub fn stop_pre_encoding(&mut self) {
    assert!(self.pre_encoding);

    self.current_byte_index = self.booked_byte_index;
    self.current_bit_index = self.booked_bit_index;

    self.current_padding = self.booked_skipped_steps;
    self.offset = self.booked_offset;

    self.pre_encoding = false;
  }

  pub fn start_final_encoding(&mut self) {
    self.final_encoding = true;
  }

  pub fn stop_final_encoding(&mut self) {
    self.final_encoding = false;
  }

  pub fn is_enabled(&self) -> bool {
    self.pre_encoding || self.final_encoding
  }

  pub fn is_done(&self) -> bool {
    self.current_byte_index >= self.data.len()
  }

  pub fn inject_in_angle(&mut self, angle: u32) -> u32 {
    if !self.is_enabled() {
      return angle;
    }

    if self.current_offset < self.offset {
      self.current_offset += 1;
      return angle;
    }

    if self.is_done() {
      return angle;
    }

    if self.current_padding > 0 {
      if self.final_encoding {
        println!("[Skipping] Padding, angle: {}", angle);
      }

      self.current_padding -= 1;
      return angle;
    }

    self.current_padding = self.padding;

    if angle == 6 {
      if self.final_encoding {
        println!("[Skipping] Angle is 6");
      }
      return angle;
    }

    let sub_angle = (angle / 2) * 2;

    let injected_value: u32 = ((self.data[self.current_byte_index] as u32)
      >> self.current_bit_index)
      & 1;
    self.current_bit_index += 1;
    if self.current_bit_index == 8 {
      self.current_byte_index += 1;
      self.current_bit_index = 0;
    }

    let new_angle = sub_angle + injected_value;

    if self.final_encoding {
      println!(
        "Angle: {}, new angle: {}, injected value => {}",
        angle, new_angle, injected_value
      );
    }

    new_angle
  }
}

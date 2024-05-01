// Copyright (c) 2017-2023, The rav1e contributors. All rights reserved
//
// This source code is subject to the terms of the BSD 2 Clause License and
// the Alliance for Open Media Patent License 1.0. If the BSD 2 Clause License
// was not distributed with this source code in the LICENSE file, you can
// obtain it at www.aomedia.org/license/software. If the Alliance for Open
// Media Patent License 1.0 was not distributed with this source code in the
// PATENTS file, you can obtain it at www.aomedia.org/license/patent.

// Safety lints
#![deny(bare_trait_objects)]
#![deny(clippy::as_ptr_cast_mut)]
#![deny(clippy::large_stack_arrays)]
// Performance lints
#![warn(clippy::inefficient_to_string)]
#![warn(clippy::invalid_upcast_comparisons)]
#![warn(clippy::iter_with_drain)]
#![warn(clippy::linkedlist)]
#![warn(clippy::mutex_integer)]
#![warn(clippy::naive_bytecount)]
#![warn(clippy::needless_bitwise_bool)]
#![warn(clippy::needless_collect)]
#![warn(clippy::or_fun_call)]
#![warn(clippy::stable_sort_primitive)]
#![warn(clippy::suboptimal_flops)]
#![warn(clippy::trivial_regex)]
#![warn(clippy::trivially_copy_pass_by_ref)]
#![warn(clippy::unnecessary_join)]
#![warn(clippy::unused_async)]
#![warn(clippy::zero_sized_map_values)]
// Correctness lints
#![deny(clippy::case_sensitive_file_extension_comparisons)]
#![deny(clippy::copy_iterator)]
#![deny(clippy::expl_impl_clone_on_copy)]
#![deny(clippy::float_cmp)]
#![warn(clippy::imprecise_flops)]
#![deny(clippy::manual_instant_elapsed)]
#![deny(clippy::mem_forget)]
#![deny(clippy::path_buf_push_overwrite)]
#![deny(clippy::same_functions_in_if_condition)]
#![deny(clippy::unchecked_duration_subtraction)]
#![deny(clippy::unicode_not_nfc)]
// Clarity/formatting lints
#![warn(clippy::checked_conversions)]
#![allow(clippy::comparison_chain)]
#![warn(clippy::derive_partial_eq_without_eq)]
#![allow(clippy::enum_variant_names)]
#![warn(clippy::explicit_deref_methods)]
#![warn(clippy::filter_map_next)]
#![warn(clippy::flat_map_option)]
#![warn(clippy::fn_params_excessive_bools)]
#![warn(clippy::implicit_clone)]
#![warn(clippy::iter_not_returning_iterator)]
#![warn(clippy::iter_on_empty_collections)]
#![warn(clippy::macro_use_imports)]
#![warn(clippy::manual_clamp)]
#![warn(clippy::manual_let_else)]
#![warn(clippy::manual_ok_or)]
#![warn(clippy::manual_string_new)]
#![warn(clippy::map_flatten)]
#![warn(clippy::match_bool)]
#![warn(clippy::mut_mut)]
#![warn(clippy::needless_borrow)]
#![warn(clippy::needless_continue)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::too_many_arguments)]
#![warn(clippy::range_minus_one)]
#![warn(clippy::range_plus_one)]
#![warn(clippy::ref_binding_to_reference)]
#![warn(clippy::ref_option_ref)]
#![warn(clippy::trait_duplication_in_bounds)]
#![warn(clippy::unused_peekable)]
#![warn(clippy::unused_rounding)]
#![warn(clippy::unused_self)]
#![allow(clippy::upper_case_acronyms)]
#![warn(clippy::verbose_bit_mask)]
#![warn(clippy::verbose_file_reads)]
// Documentation lints
#![warn(clippy::doc_link_with_quotes)]
#![warn(clippy::doc_markdown)]
#![warn(clippy::missing_errors_doc)]
#![warn(clippy::missing_panics_doc)]

#[macro_use]
extern crate log;

mod common;
mod decoder;
mod error;
#[cfg(feature = "serialize")]
mod kv;
mod muxer;
mod stats;

use crate::common::*;
use crate::error::*;
use crate::stats::*;
use chacha20::ChaCha20;
use rav1e::config::CpuFeatureLevel;
use rav1e::prelude::*;
use rav1e::steg::Hic;
use rav1e::steg::InnerHic;
use x25519_dalek::StaticSecret;
use x25519_dalek::{EphemeralSecret, PublicKey, SharedSecret};

use crate::decoder::{Decoder, FrameBuilder, VideoDetails};
use crate::muxer::*;
use std::fs::File;
use std::io::{Read, Seek, Write};
use std::net::TcpListener;
use std::net::TcpStream;
use std::process::exit;
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::thread::JoinHandle;

impl<T: Pixel> FrameBuilder<T> for Context<'_, T> {
  fn new_frame(&self) -> Frame<T> {
    Context::new_frame(self)
  }
}

struct Source<D: Decoder> {
  limit: usize,
  count: usize,
  input: D,
  #[cfg(all(unix, feature = "signal-hook"))]
  exit_requested: Arc<std::sync::atomic::AtomicBool>,
}

impl<D: Decoder> Source<D> {
  cfg_if::cfg_if! {
    if #[cfg(all(unix, feature = "signal-hook"))] {
      fn new(limit: usize, input: D) -> Self {
        use signal_hook::{flag, consts};

        // Make sure double CTRL+C and similar kills
        let exit_requested = Arc::new(std::sync::atomic::AtomicBool::new(false));
        for sig in consts::TERM_SIGNALS {
            // When terminated by a second term signal, exit with exit code 1.
            // This will do nothing the first time (because term_now is false).
            flag::register_conditional_shutdown(*sig, 1, Arc::clone(&exit_requested)).unwrap();
            // But this will "arm" the above for the second time, by setting it to true.
            // The order of registering these is important, if you put this one first, it will
            // first arm and then terminate ‒ all in the first round.
            flag::register(*sig, Arc::clone(&exit_requested)).unwrap();
        }

        Self { limit, input, count: 0, exit_requested, }
      }
    } else {
      #[allow(clippy::missing_const_for_fn)]
      fn new(limit: usize, input: D) -> Self {
        Self { limit, input, count: 0, }
      }
    }
  }

  #[profiling::function]
  fn read_frame<T: Pixel>(
    &mut self, ctx: &mut Context<T>, video_info: VideoDetails,
  ) -> Result<(), CliError> {
    if self.limit != 0 && self.count == self.limit {
      ctx.flush();
      return Ok(());
    }

    #[cfg(all(unix, feature = "signal-hook"))]
    {
      if self.exit_requested.load(std::sync::atomic::Ordering::SeqCst) {
        ctx.flush();
        return Ok(());
      }
    }

    match self.input.read_frame(ctx, &video_info) {
      Ok(frame) => {
        match video_info.bit_depth {
          8 | 10 | 12 => {}
          _ => return Err(CliError::new("Unsupported bit depth")),
        }
        self.count += 1;
        let _ = ctx.send_frame(Some(Arc::new(frame)));
      }
      _ => {
        ctx.flush();
      }
    };
    Ok(())
  }
}

// Encode and write a frame.
// Returns frame information in a `Result`.
#[profiling::function]
fn process_frame<T: Pixel, D: Decoder>(
  ctx: &mut Context<T>, output_file: &mut dyn Muxer, source: &mut Source<D>,
  pass1file: Option<&mut File>, pass2file: Option<&mut File>,
  mut y4m_enc: Option<&mut y4m::Encoder<Box<dyn Write + Send>>>,
  metrics_cli: MetricsEnabled,
) -> Result<Option<Vec<FrameSummary>>, CliError> {
  let y4m_details = source.input.get_video_details();
  let mut frame_summaries = Vec::new();
  let mut pass1file = pass1file;
  let mut pass2file = pass2file;

  // Submit first pass data to pass 2.
  if let Some(passfile) = pass2file.as_mut() {
    while ctx.rc_second_pass_data_required() > 0 {
      let mut buflen = [0u8; 8];
      passfile
        .read_exact(&mut buflen)
        .map_err(|e| e.context("Unable to read the two-pass data file."))?;
      let mut data = vec![0u8; u64::from_be_bytes(buflen) as usize];

      passfile
        .read_exact(&mut data)
        .map_err(|e| e.context("Unable to read the two-pass data file."))?;

      ctx
        .rc_send_pass_data(&data)
        .map_err(|e| e.context("Corrupted first pass data"))?;
    }
  }

  let pkt_wrapped = ctx.receive_packet();
  let (ret, emit_pass_data) = match pkt_wrapped {
    Ok(pkt) => {
      output_file.write_frame(
        pkt.input_frameno,
        pkt.data.as_ref(),
        pkt.frame_type,
      );
      if let (Some(ref mut y4m_enc_uw), Some(ref rec)) =
        (y4m_enc.as_mut(), &pkt.rec)
      {
        write_y4m_frame(y4m_enc_uw, rec, y4m_details);
      }
      frame_summaries.push(build_frame_summary(
        pkt,
        y4m_details.bit_depth,
        y4m_details.chroma_sampling,
        metrics_cli,
      ));
      Ok((Some(frame_summaries), true))
    }
    Err(EncoderStatus::NeedMoreData) => {
      source.read_frame(ctx, y4m_details)?;
      Ok((Some(frame_summaries), false))
    }
    Err(EncoderStatus::EnoughData) => {
      unreachable!()
    }
    Err(EncoderStatus::LimitReached) => Ok((None, true)),
    Err(e @ EncoderStatus::Failure) => {
      Err(e.context("Failed to encode video"))
    }
    Err(e @ EncoderStatus::NotReady) => {
      Err(e.context("Mismanaged handling of two-pass stats data"))
    }
    Err(EncoderStatus::Encoded) => Ok((Some(frame_summaries), true)),
  }?;

  // Save first pass data from pass 1.
  if let Some(passfile) = pass1file.as_mut() {
    if emit_pass_data {
      match ctx.rc_receive_pass_data() {
        Some(RcData::Frame(outbuf)) => {
          let len = outbuf.len() as u64;
          passfile.write_all(&len.to_be_bytes()).map_err(|e| {
            e.context("Unable to write to two-pass data file.")
          })?;

          passfile.write_all(&outbuf).map_err(|e| {
            e.context("Unable to write to two-pass data file.")
          })?;
        }
        Some(RcData::Summary(outbuf)) => {
          // The last packet of rate control data we get is the summary data.
          // Let's put it at the start of the file.
          passfile.rewind().map_err(|e| {
            e.context("Unable to seek in the two-pass data file.")
          })?;
          let len = outbuf.len() as u64;

          passfile.write_all(&len.to_be_bytes()).map_err(|e| {
            e.context("Unable to write to two-pass data file.")
          })?;

          passfile.write_all(&outbuf).map_err(|e| {
            e.context("Unable to write to two-pass data file.")
          })?;
        }
        None => {}
      }
    }
  }

  Ok(ret)
}

fn do_encode<T: Pixel, D: Decoder>(
  cfg: Config, verbose: Verboseness, mut progress: ProgressInfo,
  output: &mut dyn Muxer, mut source: Source<D>, mut pass1file: Option<File>,
  mut pass2file: Option<File>,
  mut y4m_enc: Option<y4m::Encoder<Box<dyn Write + Send>>>,
  metrics_enabled: MetricsEnabled, mut hidden_information_container: Hic,
) -> Result<(), CliError> {
  let mut ctx: Context<T> = cfg
    .new_context(&mut hidden_information_container)
    .map_err(|e| e.context("Invalid encoder settings"))?;

  // Let's write down a placeholder.
  if let Some(passfile) = pass1file.as_mut() {
    let len = ctx.rc_summary_size();
    let buf = vec![0u8; len];

    passfile
      .write_all(&(len as u64).to_be_bytes())
      .map_err(|e| e.context("Unable to write to two-pass data file."))?;

    passfile
      .write_all(&buf)
      .map_err(|e| e.context("Unable to write to two-pass data file."))?;
  }

  while let Some(frame_info) = process_frame(
    &mut ctx,
    &mut *output,
    &mut source,
    pass1file.as_mut(),
    pass2file.as_mut(),
    y4m_enc.as_mut(),
    metrics_enabled,
  )? {
    if verbose != Verboseness::Quiet {
      for frame in frame_info {
        progress.add_frame(frame.clone());
        if verbose == Verboseness::Verbose {
          info!("{} - {}", frame, progress);
        } else {
          // Print a one-line progress indicator that overrides itself with every update
          eprint!("\r{progress}                    ");
        };
      }

      output.flush().unwrap();
    }
  }
  if verbose != Verboseness::Quiet {
    if verbose == Verboseness::Verbose {
      // Clear out the temporary progress indicator
      eprint!("\r");
    }
    progress.print_summary(verbose == Verboseness::Verbose);
  }
  Ok(())
}

// 1. Create fake RTSP server
//   a. TCP socket with designated commands
//   b. 0 -> Set param command (used by client to inject their secret)
//   c. 1 -> Start playback
//   d. 2 -> Pause playback
// 2. Encoder send mixture in first 256 bits, then encrypted payload file
//    after
// 3. Decoder (handle decryption)
//   - Have dav1d write raw bit angles to unix socket
//   - Create wrapper program that connects to fake RTSP server, sends params,
//     starts dav1d decoder, reads bits, computes shared secret, decripts
//
//
// 1. Client generates diffie helman secret
// 2. Client initates TCP connection to server
// 3. Client calls set param with mixture
//   a. Server generates helman secret
//   b. Server computes shared secret
//   c. Server waits for client to start playback
// 4. Client calls start playback
//   a. Server uses up to first 256 bits to share its mixture
//   b. After first 256 bits, server reads actual payload file, and encrypts
//   it with shared secret
// 5. Client receives server mixture
// 6. Client computes shared secret
// 7. After first 256 angle bits received, client starts decrypting bitstream

struct EncryptionData {
  shared_secret: zeroize::Zeroizing<SharedSecret>,
  server_public: PublicKey,
  socket: TcpStream,
}

fn start_rtsp_task(
  tx: SyncSender<EncryptionData>,
) -> JoinHandle<anyhow::Result<()>> {
  std::thread::spawn(move || {
    let s = TcpListener::bind("127.0.0.1:6969").unwrap();
    println!("Waiting for client to connect");
    let (mut client, client_addr) = s.accept().unwrap();
    println!("Client connected at {client_addr:?}");
    let mut cmd = [0u8; 1];
    client.read_exact(&mut cmd).unwrap();
    assert_eq!(
      cmd[0], 0,
      "Client didnt send set param opcode. Expected 0, got {}",
      cmd[0]
    );
    let mut param_length = [0u8; 2];
    client.read_exact(&mut param_length).unwrap();
    let param_length = u16::from_le_bytes(param_length);

    let mut param = vec![0u8; param_length as usize];
    client.read_exact(&mut param).unwrap();

    println!("Got param: {param:?}");
    assert!(param.len() >= 32, "Set param must be at least 32 bytes");
    let mut client_public = [0u8; 32];
    client_public.copy_from_slice(&param[..32]);

    let client_secret = StaticSecret::from([0u8; 32]);
    let client_public = PublicKey::from(&client_secret);

    //let server_secret = EphemeralSecret::random();
    let server_secret = StaticSecret::from([0u8; 32]);
    let server_public = PublicKey::from(&server_secret);

    let shared_secret = server_secret.diffie_hellman(&client_public);

    let mut cmd = [0u8; 1];
    client.read_exact(&mut cmd).unwrap();
    assert_eq!(
      cmd[0], 1,
      "Client didnt send start opcode after set param. Expected 1, got {}",
      cmd[0]
    );

    let data = EncryptionData {
      shared_secret: shared_secret.into(),
      server_public,
      socket: client,
    };

    tx.send(data).expect("Only sent once");

    Ok(())
  })
}

fn main() {
  init_logger();

  #[cfg(feature = "tracing")]
  let (chrome_layer, _guard) =
    tracing_chrome::ChromeLayerBuilder::new().build();

  #[cfg(feature = "tracing")]
  {
    use tracing_subscriber::layer::SubscriberExt;
    tracing::subscriber::set_global_default(
      tracing_subscriber::registry().with(chrome_layer),
    )
    .unwrap();
  }

  run().unwrap_or_else(|e| {
    error::print_error(&e);
    exit(1);
  });
}

fn init_logger() {
  use std::str::FromStr;
  fn level_colored(l: log::Level) -> console::StyledObject<&'static str> {
    use console::style;
    use log::Level;
    match l {
      Level::Trace => style("??").dim(),
      Level::Debug => style("? ").dim(),
      Level::Info => style("> ").green(),
      Level::Warn => style("! ").yellow(),
      Level::Error => style("!!").red(),
    }
  }

  let level = std::env::var("RAV1E_LOG")
    .ok()
    .and_then(|l| log::LevelFilter::from_str(&l).ok())
    .unwrap_or(log::LevelFilter::Info);

  fern::Dispatch::new()
    .format(move |out, message, record| {
      out.finish(format_args!(
        "{level} {message}",
        level = level_colored(record.level()),
        message = message,
      ));
    })
    // set the default log level. to filter out verbose log messages from dependencies, set
    // this to Warn and overwrite the log level for your crate.
    .level(log::LevelFilter::Warn)
    // change log levels for individual modules. Note: This looks for the record's target
    // field which defaults to the module path but can be overwritten with the `target`
    // parameter:
    // `info!(target="special_target", "This log message is about special_target");`
    .level_for("rav1e", level)
    // output to stdout
    .chain(std::io::stderr())
    .apply()
    .unwrap();
}

cfg_if::cfg_if! {
  if #[cfg(any(target_os = "windows", target_arch = "wasm32"))] {
    fn print_rusage() {
      eprintln!("Resource usage reporting is not currently supported on this platform");
    }
  } else {
    fn print_rusage() {
      // SAFETY: This uses an FFI, it is safe because we call it correctly.
      let (utime, stime, maxrss) = unsafe {
        let mut usage = std::mem::zeroed();
        let _ = libc::getrusage(libc::RUSAGE_SELF, &mut usage);
        (usage.ru_utime, usage.ru_stime, usage.ru_maxrss)
      };
      eprintln!(
        "user time: {} s",
        utime.tv_sec as f64 + utime.tv_usec as f64 / 1_000_000f64
      );
      eprintln!(
        "system time: {} s",
        stime.tv_sec as f64 + stime.tv_usec as f64 / 1_000_000f64
      );
      eprintln!("maximum rss: {maxrss} KB");
    }
  }
}

fn run() -> Result<(), error::CliError> {
  let (tx, rx) = std::sync::mpsc::sync_channel(1);
  let thread = start_rtsp_task(tx);

  let data = rx.recv().expect("Networking task ended without sending data");
  thread.join().unwrap().expect("Networking task failed");

  println!("Main got server pub {:?}", data.server_public);

  let output: Box<dyn std::io::Write + Send> = Box::new(data.socket);
  let mut cli = parse_cli(Some(output))?;

  // Maximum frame size by specification + maximum y4m header
  let limit = y4m::Limits {
    // Use saturating operations to gracefully handle 32-bit architectures
    bytes: 64usize
      .saturating_mul(64)
      .saturating_mul(4096)
      .saturating_mul(2304)
      .saturating_add(1024),
  };
  let mut y4m_dec = match y4m::Decoder::new_with_limits(cli.io.input, limit) {
    Err(e) => {
      return Err(CliError::new(match e {
        y4m::Error::ParseError(_) => {
          "Could not parse input video. Is it a y4m file?"
        }
        y4m::Error::IoError(_) => {
          "Could not read input file. Check that the path is correct and you have read permissions."
        }
        y4m::Error::UnknownColorspace => {
          "Unknown colorspace or unsupported bit depth."
        }
        y4m::Error::OutOfMemory => "The video's frame size exceeds the limit.",
        y4m::Error::EOF => "Unexpected end of input.",
        y4m::Error::BadInput => "Bad y4m input parameters provided.",
      }))
    }
    Ok(d) => d,
  };
  let video_info = y4m_dec.get_video_details();
  let y4m_enc = cli.io.rec.map(|rec| {
    y4m::encode(
      video_info.width,
      video_info.height,
      y4m::Ratio::new(
        video_info.time_base.den as usize,
        video_info.time_base.num as usize,
      ),
    )
    .with_colorspace(y4m_dec.get_colorspace())
    .with_pixel_aspect(y4m::Ratio {
      num: video_info.sample_aspect_ratio.num as usize,
      den: video_info.sample_aspect_ratio.den as usize,
    })
    .write_header(rec)
    .unwrap()
  });

  cli.enc.width = video_info.width;
  cli.enc.height = video_info.height;
  cli.enc.sample_aspect_ratio = video_info.sample_aspect_ratio;
  cli.enc.bit_depth = video_info.bit_depth;
  cli.enc.chroma_sampling = video_info.chroma_sampling;
  cli.enc.chroma_sample_position = video_info.chroma_sample_position;

  // If no pixel range is specified via CLI, assume limited,
  // as it is the default for the Y4M format.
  if !cli.color_range_specified {
    cli.enc.pixel_range = PixelRange::Limited;
  }

  if !cli.override_time_base {
    cli.enc.time_base = video_info.time_base;
  }

  if cli.photon_noise > 0 && cli.enc.film_grain_params.is_none() {
    cli.enc.film_grain_params = Some(vec![generate_photon_noise_params(
      0,
      u64::MAX,
      NoiseGenArgs {
        iso_setting: cli.photon_noise as u32 * 100,
        width: video_info.width as u32,
        height: video_info.height as u32,
        transfer_function: if cli.enc.is_hdr() {
          TransferFunction::SMPTE2084
        } else {
          TransferFunction::BT1886
        },
        chroma_grain: false,
        random_seed: None,
      },
    )]);
  }

  let mut rc = RateControlConfig::new();

  let pass2file = match cli.pass2file_name {
    Some(f) => {
      let mut f = File::open(f).map_err(|e| {
        e.context("Unable to open file for reading two-pass data")
      })?;
      let mut buflen = [0u8; 8];
      f.read_exact(&mut buflen)
        .map_err(|e| e.context("Summary data too short"))?;
      let len = i64::from_be_bytes(buflen);
      let mut buf = vec![0u8; len as usize];

      f.read_exact(&mut buf)
        .map_err(|e| e.context("Summary data too short"))?;

      rc = RateControlConfig::from_summary_slice(&buf)
        .map_err(|e| e.context("Invalid summary"))?;

      Some(f)
    }
    None => None,
  };

  let pass1file = match cli.pass1file_name {
    Some(f) => {
      let f = File::create(f).map_err(|e| {
        e.context("Unable to open file for writing two-pass data")
      })?;
      rc = rc.with_emit_data(true);
      Some(f)
    }
    None => None,
  };

  let cfg = Config::new()
    .with_encoder_config(cli.enc.clone())
    .with_threads(cli.threads)
    .with_rate_control(rc);

  #[cfg(feature = "serialize")]
  {
    if let Some(save_config) = cli.save_config {
      let mut out = File::create(save_config)
        .map_err(|e| e.context("Cannot create configuration file"))?;
      let s = toml::to_string(&cli.enc).unwrap();
      out
        .write_all(s.as_bytes())
        .map_err(|e| e.context("Cannot write the configuration file"))?
    }
  }

  cli.io.output.write_header(
    video_info.width,
    video_info.height,
    cli.enc.time_base.den as usize,
    cli.enc.time_base.num as usize,
  );

  let tiling =
    cfg.tiling_info().map_err(|e| e.context("Invalid configuration"))?;
  if cli.verbose != Verboseness::Quiet {
    info!("CPU Feature Level: {}", CpuFeatureLevel::default());

    info!(
      "Using y4m decoder: {}x{}p @ {}/{} fps, {}, {}-bit",
      video_info.width,
      video_info.height,
      video_info.time_base.den,
      video_info.time_base.num,
      video_info.chroma_sampling,
      video_info.bit_depth
    );
    info!("Encoding settings: {}", cli.enc);

    if tiling.tile_count() == 1 {
      info!("Using 1 tile");
    } else {
      info!(
        "Using {} tiles ({}x{})",
        tiling.tile_count(),
        tiling.cols,
        tiling.rows
      );
    }
  }

  let progress = ProgressInfo::new(
    Rational { num: video_info.time_base.den, den: video_info.time_base.num },
    if cli.limit == 0 { None } else { Some(cli.limit) },
    cli.metrics_enabled,
  );

  for _ in 0..cli.skip {
    match y4m_dec.read_frame() {
      Ok(f) => f,
      Err(_) => {
        return Err(CliError::new("Skipped more frames than in the input"))
      }
    };
  }

  let source = Source::new(cli.limit, y4m_dec);

  let hic: Hic = match cli.hidden_information_config {
    None => Hic::just_data(vec![], None, None),
    Some(config) => {
      let mut payload_bytes = config.string.into_bytes();
      payload_bytes.push(0b0);

      let nonce = [0x24; 12];
      use chacha20::cipher::{KeyIvInit, StreamCipher};

      let mut cipher =
        ChaCha20::new(data.shared_secret.as_bytes().into(), &nonce.into());

      cipher.apply_keystream(&mut payload_bytes);
      let server_public =
        data.server_public.as_bytes().iter().copied().collect();
      let pubkey = InnerHic::new(server_public, config.padding, config.offset);

      let payload =
        InnerHic::new(payload_bytes, config.padding, config.offset);

      Hic::new(pubkey, payload)
    }
  };

  if video_info.bit_depth == 8 && !cli.force_highbitdepth {
    do_encode::<u8, y4m::Decoder<Box<dyn Read + Send>>>(
      cfg,
      cli.verbose,
      progress,
      &mut *cli.io.output,
      source,
      pass1file,
      pass2file,
      y4m_enc,
      cli.metrics_enabled,
      hic,
    )?
  } else {
    do_encode::<u16, y4m::Decoder<Box<dyn Read + Send>>>(
      cfg,
      cli.verbose,
      progress,
      &mut *cli.io.output,
      source,
      pass1file,
      pass2file,
      y4m_enc,
      cli.metrics_enabled,
      hic,
    )?
  }
  if cli.benchmark {
    print_rusage();
  }

  Ok(())
}

// Audio passthrough example for daisy seed
// Currently support for WM8731 codec and PCM3060 codec
// For WM8731 use feature "seed_1_1"
// For PCM3060 use feature "seed_1_2"
//
// Just like they did in https://github.com/zlosynth/daisy
#![no_std]
#![no_main]

use core::num::Wrapping;
use daisy_embassy::{
    DaisyBoard, audio::HALF_DMA_BUFFER_LENGTH, hal, led::UserLed, new_daisy_board,
};
use defmt::{debug, unwrap};
use embassy_executor::Spawner;
use embassy_stm32::{bind_interrupts, dma};
use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::Timer;
use faust_types::ParamIndex;
use {defmt_rtt as _, panic_probe as _};

mod dsp;
use dsp::Volumecontrol;

static SHARED_VOLUME: Signal<ThreadModeRawMutex, f32> = Signal::new();

bind_interrupts!(
    pub struct Irqs{
        DMA1_STREAM0 => dma::InterruptHandler<embassy_stm32::peripherals::DMA1_CH0>;
        DMA1_STREAM1 => dma::InterruptHandler<embassy_stm32::peripherals::DMA1_CH1>;
        DMA1_STREAM2 => dma::InterruptHandler<embassy_stm32::peripherals::DMA1_CH2>;
});

#[embassy_executor::task]
async fn blink(mut led: UserLed<'static>) {
    // Blink LED while audio passthrough to show sign of life
    // Also slowly increase the volume and then reset it in a 10s cycle.

    let mut volume = 1.0_f32;

    loop {
        if volume > 10.0 {
            volume = 1.0;
        } else {
            volume = volume + 1.0;
        }

        led.on();
        SHARED_VOLUME.signal(volume);
        Timer::after_millis(500).await;

        led.off();
        SHARED_VOLUME.signal(volume);
        Timer::after_millis(500).await;
    }
}

// see https://github.com/zlosynth/daisy
// Convert audio PCM data from u24 to f32,
#[inline(always)]
fn u24_to_f32(y: u32) -> f32 {
    let y = (Wrapping(y) + Wrapping(0x0080_0000)).0 & 0x00FF_FFFF; // convert to i32
    (y as f32 / 8_388_608.0) - 1.0 // (2^24) / 2
}

// Convert audio data from f32 to u24 PCM
#[inline(always)]
fn f32_to_u24(x: f32) -> u32 {
    let x = x * 8_388_607.0;
    let x = x.clamp(-8_388_608.0, 8_388_607.0);
    (x as i32) as u32
}

fn process_audio_faust(dsp: &mut Volumecontrol, input: &[u32], output: &mut [u32]) {
    // would prefer to have fewer buffer copies.
    let mut fbuf = [
        [0.0_f32; HALF_DMA_BUFFER_LENGTH],
        [0.0_f32; HALF_DMA_BUFFER_LENGTH],
    ];
    let mut fbuf2 = [
        [0.0_f32; HALF_DMA_BUFFER_LENGTH],
        [0.0_f32; HALF_DMA_BUFFER_LENGTH],
    ];

    for (i, u32_value) in input.iter().enumerate() {
        // Extract integer part and scale it to f32
        fbuf[0][i] = u24_to_f32(*u32_value);
        fbuf[1][i] = u24_to_f32(*u32_value);
    }

    // if a new value is recieved, set it.
    // only checked once per buffer copy
    let volume = SHARED_VOLUME.try_take().unwrap_or(9999.9);
    if volume != 9999.9 {
        dsp.set_param(ParamIndex(1), volume);
    }

    dsp.compute(HALF_DMA_BUFFER_LENGTH, &fbuf, &mut fbuf2);

    for (i, f32_value) in fbuf2[0].iter().enumerate() {
        output[i] = f32_to_u24(*f32_value);
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    debug!("====program start====");
    let config = daisy_embassy::default_rcc();
    let p = hal::init(config);
    let board: DaisyBoard<'_> = new_daisy_board!(p);

    // DSP is the Faust DSP Function
    // see rust-faust minimal example main.rs
    let mut dsp: Volumecontrol = Volumecontrol::new();
    dsp.init(48_000);
    dsp.instance_reset_params();
    dsp.set_param(ParamIndex(1), 1.0_f32);
    SHARED_VOLUME.signal(1.0_f32);

    let led = board.user_led;
    spawner.spawn(blink(led)).unwrap();

    let interface = board
        .audio_peripherals
        .prepare_interface(Default::default(), Irqs)
        .await;

    let mut interface = unwrap!(interface.start_interface().await);

    // use a closure to pass the volume control dsp into the callback.
    unwrap!(
        interface
            .start_callback(|input: &[u32], output: &mut [u32]| {
                process_audio_faust(&mut dsp, input, output)
            })
            .await
    );
}

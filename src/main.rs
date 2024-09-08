
use std::collections::HashSet;
use std::env::args;
use std::io::{Cursor, Write};

use ab_glyph::{FontRef, PxScale};
use futures::stream::SplitSink;
use futures::{SinkExt, StreamExt};
use imageproc::definitions::Image;
use imageproc::image::Rgb;
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

mod gpu;

#[derive(Deserialize)]
struct StreamDeckInboundEvent {
    action: String,
    context: String,
    device: String,
    event: String,

}

/// The data received from Stream Deck application during plugin registration.
struct StreamDeckInfo {
    port: u32,
    uuid: String,
    event_type: String,
    info: String, // JSON string of additional info
}

struct ApplicationState {
    gpu_info_context_ids: HashSet<String>,
    nvml: nvml_wrapper::Nvml,
}

impl Default for ApplicationState {
    fn default() -> Self {
        Self{ 
            gpu_info_context_ids: HashSet::new(),
            nvml: nvml_wrapper::Nvml::init().unwrap(),
        }
    }
}

#[tokio::main]
async fn main() {
    let args = args();
    let arg_vec: Vec<String> = args.collect();
    let folded_args = arg_vec.iter().fold(String::new(), |mut a, v| {
        a.push_str(format!("{0},", v).as_str());
        return a;
    });
    println!("{0}", folded_args);
    log_stuff(folded_args);

    // Get the individual values passed in from Stream Deck application
    let sd_info = StreamDeckInfo{
        info: arg_vec.get(8).expect("Failed to get additional plugin info input").to_owned(),
        event_type: arg_vec.get(6).expect("Failed to get event type for plugin registration").to_owned(),
        uuid: arg_vec.get(4).expect("Failed to get UUID for plugin registration").to_owned(),
        port: arg_vec.get(2).expect("Failed to get port").to_owned().parse::<u32>().expect("Failed conversion of port from String to u32"),
    };
    println!(" Port: {0}", sd_info.port);
    println!(" UUID: {0}", sd_info.uuid);
    println!("Event: {0}", sd_info.event_type);
    println!(" Info: {0}", sd_info.info);

    log_stuff("Attempting to open websocket connection".to_string());
    let uri = format!("ws://127.0.0.1:{0}", sd_info.port);
    let (ws_connection, _) = connect_async(uri).await.expect("Failed to open websocket connection");
    let (mut ws_write, mut ws_read) = ws_connection.split();
    log_stuff("Completed websocket connection successfully".to_string());

    let message = format!(r#"{{"uuid": "{0}", "event": "{1}"}}"#, sd_info.uuid, sd_info.event_type);
    let write_result = ws_write.send(Message::Text(message)).await;
    if write_result.is_err() {
        log_stuff(format!("Failed to send message: {0}", write_result.err().unwrap()));
    }    
    log_stuff("Successfully registered with Stream Deck device".to_string());

    // Initialize application state
    let mut app_state = ApplicationState::default();

    loop {
        let read_message = tokio::time::timeout(std::time::Duration::from_millis(2000), ws_read.next()).await.unwrap_or_default();
        match read_message {
            Some(msg) => {
                match msg {
                    Ok(msg) => {
                        let msg_text = msg.into_text().unwrap();
                        log_stuff(format!("Received message: {0}", msg_text));
                        process_message(msg_text, &mut app_state);
                        
                    }
                    Err(err) => { log_stuff(format!("Error occurred while reading message from websocket stream: {0}", err).to_string()) }
                }
            }
            None => {
                log_stuff("No websocket message found".to_string());
            }
        }
        send_base64_image(&mut ws_write, get_gpu_image(&app_state), &app_state).await;
    }

    // let nvml = Nvml::init().expect("Failed to initialize the NVIDIA Management Library (NVML)");

}

fn log_stuff(message: String) {
    let mut log_file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .write(true)
        .open("log.txt")
        .expect("Failed to open log file");

    writeln!(log_file, "{0}", message).expect("Failed to write to log file");
}

/// Retrieves a Base64 string containing an image with GPU information
fn get_gpu_image(app_state: &ApplicationState) -> String {
    let mut image = Image::new(256,256);

    let scale = PxScale {x: 50f32, y: 50f32};

    let font = FontRef::try_from_slice(include_bytes!("../assets/Tomorrow-Medium.ttf")).unwrap();
    
    let gpu = app_state.nvml.device_by_index(0).unwrap();
    let temp = gpu.temperature(nvml_wrapper::enum_wrappers::device::TemperatureSensor::Gpu).unwrap();
    let gpu_clock = gpu.clock_info(nvml_wrapper::enum_wrappers::device::Clock::Graphics).unwrap();
    let watts = gpu.power_usage().unwrap();
    let mem_clock = gpu.clock_info(nvml_wrapper::enum_wrappers::device::Clock::Memory).unwrap();

    imageproc::drawing::draw_text_mut(&mut image, Rgb([100u8, 100u8, 255u8]), 10, 10, scale, &font, format!("Temp: {0}", temp).as_str());
    imageproc::drawing::draw_text_mut(&mut image, Rgb([100u8, 100u8, 255u8]), 10, 55, scale, &font, format!("Clock: {0}", gpu_clock).as_str());
    imageproc::drawing::draw_text_mut(&mut image, Rgb([100u8, 100u8, 255u8]), 10, 100, scale, &font, format!("Watts: {0}", (watts/1000) as u8).as_str());
    imageproc::drawing::draw_text_mut(&mut image, Rgb([100u8, 100u8, 255u8]), 10, 145, scale, &font, format!("MClk: {0}", mem_clock).as_str());
    


    let mut png_bytes: Vec<u8> = Vec::new();
    let mut cursor = Cursor::new(&mut png_bytes);

    image.write_to(&mut cursor, imageproc::image::ImageFormat::Png);

    return STANDARD.encode(png_bytes);
}

/// Sends a Base64 image payload to a specific context (icon instance) in Stream Deck
async fn send_base64_image(ws_writer: &mut SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>, image: String, app_state: &ApplicationState) {
    log_stuff("Sending image to Stream Deck icon instances...".to_string());
    for context_id in &app_state.gpu_info_context_ids {
        let payload = format!(r#"
        {{
            "event": "setImage",
            "context": "{0}",
            "payload": {{
                "image": "data:image/png;base64,{1}",
                "target": "both"
            }}
        }}
        "#, context_id, image);
        let send_result = ws_writer.send(Message::text(payload)).await;

        match send_result {
            Err(err) => {
                log_stuff(format!("Failed to send image to context ID {0}. Error: {1}", context_id, err.to_string()));
            }
            Ok(_) => {
                log_stuff(format!("Successfully sent image to context ID {0}", context_id));
            }
        }
    }
}

// Handles various types of inbound events from Stream Deck software
fn process_message(message: String, app_state: &mut ApplicationState) {
    let event: Result<StreamDeckInboundEvent, serde_json::Error> = serde_json::from_str(message.as_str());
    match event {
        Ok(event) => {
            if event.event == "willAppear" && event.action == "net.trevorsullivan.trevor.gpu.state" {
                app_state.gpu_info_context_ids.insert(event.context.clone());
                log_stuff(format!("Added context {0} to application state (gpu_info_context_ids)", event.context));
            }
            if event.event == "willDisappear" && event.action == "net.trevorsullivan.trevor.gpu.state" {
                app_state.gpu_info_context_ids.remove(&event.context);
                log_stuff(format!("Removed context {0} from application state (gpu_info_context_ids)", event.context));
            }
        }
        Err(err) => {
            log_stuff(format!("Could not deseralize this event: {0}, error: {1}", message, err.to_string()));
        }
    }
}
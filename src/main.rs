#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![feature(iter_next_chunk)]

extern crate alloc;
use alloc::{
    fmt, format, string::{String, ToString}, vec::Vec,
};
use core::{
    iter::Peekable,
    slice::Iter,
    fmt::Debug, 
    mem::MaybeUninit
};


use static_cell::make_static;

use esp32_hal::{
    clock::ClockControl, 
    efuse::Efuse, 
    peripherals::Peripherals, 
    prelude::*, 
    timer::TimerGroup, 
    Rng, 
    Rtc,
    embassy,
};

use esp_wifi::{
    esp_now::{
        enable_esp_now_with_wifi,
        EspNow,
        EspNowManager, 
        EspNowReceiver, 
        EspNowSender,
        PeerInfo, 
        BROADCAST_ADDRESS,
        ReceivedData
    },
    initialize, 
    wifi::{
        WifiController, 
        WifiDevice, 
        WifiEvent, 
        WifiStaDevice, 
        WifiState,
        new_with_mode,
    },
    EspWifiInitFor
};

use embassy_executor::Spawner;

use embassy_net::{
    tcp::TcpSocket,
    Config, 
    Ipv4Address
};

use embassy_futures::select::{
    select3,
    Either3
};

use embassy_sync::{
    blocking_mutex::raw::CriticalSectionRawMutex, 
    channel::Channel,
    signal::Signal
};

use embassy_time::{
    Duration, 
    Timer,
    Ticker,
};

use embedded_svc::wifi::{
    ClientConfiguration, Configuration, Wifi
};

    
use esp_backtrace as _;
use esp_println as _;

use esp_println::println;
use log::{info, error, debug, warn};

use rust_mqtt::{
    client::{
        client::MqttClient, 
        client_config::ClientConfig
    },
    packet::v5::{
        reason_codes::ReasonCode,
        publish_packet::QualityOfService::QoS0,
    },

    utils::rng_generator::CountingRng,
};

const SSID: &str = env!("SSID");
const PASSWORD: &str = env!("PASSWORD");
const MQTT_ADDRESS:Ipv4Address = Ipv4Address::new(192,168,1,98);
const MQTT_PORT: u16 = 1883;
const MQTT_USERNAME: &str = env!("MQTT_USERNAME");
const MQTT_PASSWORD: &str = env!("MQTT_PASSWORD");
const MAX_MQTT_DEVICES: usize = 64;

macro_rules! format_mac {
    ($mac:expr) => {
        format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            $mac[0], $mac[1], $mac[2], $mac[3], $mac[4], $mac[5]
        )
    };
}

// #[derive(Debug)]
struct EspNowSendData {
    data: Vec<u8>,
    address: [u8; 6],
}

#[derive(Debug)]
enum DeviceError{
    InvalidMessage,
    UnknownDevice,
    UnimplementedType,
}

#[derive(Debug, PartialEq, Clone, Copy)]
struct Color {
    red: u8,
    green: u8,
    blue: u8,
}

#[repr(u32)]
#[derive(Debug, PartialEq, Clone, Copy)]
enum EspNowDevice {
    DeviceType(u8) = 0xF0,
    Motion{state:u8} = 0x21,
    Moving{state:u8} = 0x22,
    Light{color:Color,brightness:u8} = 0xFE,
}
impl Into<u8> for EspNowDevice{
    fn into(self) -> u8{
        match self{
            EspNowDevice::DeviceType(byte) => 0xF0,
            EspNowDevice::Motion{state} => 0x21,
            EspNowDevice::Moving{state} => 0x22,
            EspNowDevice::Light{color, brightness} => 0xFE,
        }
    }
}
impl EspNowDevice{
    fn try_from(byte:u8) -> Option<EspNowDevice>{
        match byte{
            0xF0 => Some(EspNowDevice::DeviceType(byte)),
            0x21 => Some(EspNowDevice::Motion{state:0}),
            0x22 => Some(EspNowDevice::Moving{state:0}),
            0xFE => Some(EspNowDevice::Light{color:Color{red:0,green:0,blue:0},brightness:0}),
            _ => None,
        }
    }
    fn as_bytes(self) -> Vec<u8>{
        match self{
            EspNowDevice::DeviceType(byte) => Vec::from([byte]),
            EspNowDevice::Motion{state} => Vec::from([Into::<u8>::into(self), state]),
            EspNowDevice::Moving{state} => Vec::from([Into::<u8>::into(self), state]),
            EspNowDevice::Light{color, brightness} => Vec::from([Into::<u8>::into(self), color.red, color.green, color.blue, brightness]),
        }
    }
    fn from_bytes(bytes: &mut Peekable<Iter<u8>>) -> Result<EspNowDevice, DeviceError>{
        let b1 = match bytes.next(){
            Some(b) => *b,
            None => return Err(DeviceError::InvalidMessage),
        };
        let mut device = match EspNowDevice::try_from(b1){
            Some(device) => device,
            None => return Err(DeviceError::UnknownDevice),
        };
        match device{
            EspNowDevice::DeviceType(_) => {
                Ok(device)
            }
            EspNowDevice::Moving{state} =>{
                let data = match bytes.next(){
                    Some(b) => *b,
                    None => return Err(DeviceError::InvalidMessage),
                };
                Ok(EspNowDevice::Moving{state:data})
            }
            _=>{
                Err(DeviceError::UnimplementedType)
            }
        }
    }
    fn as_json(&self) -> String{
        // TODO: implement json serialization
        String::new()
    }
    fn set_json(&mut self, json: &str){
        // TODO: implement json deserialization
    }
}

#[derive(PartialEq,Clone, Copy)]
struct MacAddr {
    mac: [u8; 6],
}
impl From<[u8; 6]> for MacAddr {
    fn from(mac: [u8; 6]) -> Self {
        MacAddr { mac }
    }
}
impl Debug for MacAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", self.mac[0], self.mac[1], self.mac[2], self.mac[3], self.mac[4], self.mac[5])
    }
}
impl ToString for MacAddr {
    fn to_string(&self) -> String {
        format_mac!(self.mac)
    }
}

#[derive(Debug,Clone, Copy)]
struct DeviceData {
    mac: MacAddr,
    device: EspNowDevice,
}
impl Into<EspNowSendData> for DeviceData{
    fn into(self) -> EspNowSendData{
        EspNowSendData{
            data: self.device.as_bytes(),
            address: self.mac.mac,
        }
    }
}
impl DeviceData{
    fn state_topic(&self) -> String {
        self.mac.to_string() + "/state"
    }
    fn command_topic(&self) -> String {
        self.mac.to_string() + "/set"
    }
}

#[global_allocator]
static ALLOCATOR: esp_alloc::EspHeap = esp_alloc::EspHeap::empty();
fn init_heap() {
    const HEAP_SIZE: usize = 32 * 1024;
    static mut HEAP: MaybeUninit<[u8; HEAP_SIZE]> = MaybeUninit::uninit();

    unsafe {
        ALLOCATOR.init(HEAP.as_mut_ptr() as *mut u8, HEAP_SIZE);
    }
}

#[embassy_executor::task]
async fn esp_now_sender_task(
    mut esp_now_sender: EspNowSender<'static>, 
    esp_now_send_channel: &'static Channel<CriticalSectionRawMutex, EspNowSendData,5>,
    esp_now_send_status_signal: &'static Signal<CriticalSectionRawMutex, bool>
) {
    loop {
        
        let data = esp_now_send_channel.receive().await;

        println!("Sending esp-now message to {:?}", format_mac!(data.address));
        let status = esp_now_sender.send_async(&data.address, &data.data).await;
        match status {
            Ok(()) => {
                println!("Successfully sent esp-now message");
                esp_now_send_status_signal.signal(true);
            }
            Err(e) => {
                println!("Error sending esp-now message: {:?}", e);
                esp_now_send_status_signal.signal(false);
            }
        }
    }
}

fn parse_esp_now_data(data:ReceivedData) -> Vec<DeviceData>{
    let mut new_data:Vec<DeviceData> = Vec::new();
    let mut payload = Vec::from(data.data);
    payload.truncate(data.len as usize);
    println!("Payload: {:?}", payload);
   

    let mut iter: Peekable<Iter<'_, u8>> = payload.iter().peekable();
    while let Some(_) = iter.peek() {
        let device = match EspNowDevice::from_bytes(&mut iter){
            Ok(device) => device,
            Err(e) => {
                println!("Error parsing data {:?}", e);
                break;
            }
        };
        new_data.push(DeviceData{
            mac: data.info.src_address.into(),
            device,
        });
    }
    return new_data;
}

#[embassy_executor::task]
async fn esp_now_listener_task(
    rtc: Rtc<'static>,
    manager: EspNowManager<'static>, 
    mut receiver: EspNowReceiver<'static>,
    esp_now_send_channel: &'static Channel<CriticalSectionRawMutex, EspNowSendData,5>,
    esp_now_recv_channel: &'static Channel<CriticalSectionRawMutex, Vec<DeviceData>, 5>
) {
    let mut last_time = rtc.get_time_ms();
    loop {
        let data = receiver.receive_async().await;
        

        println!("New data from: {:?}",format_mac!(data.info.src_address));
        println!("To address: {:?}",format_mac!(data.info.dst_address));
        // println!("data: {:?}",payload);
        println!("Time from last message: {:?}", rtc.get_time_ms()-last_time);
        last_time = rtc.get_time_ms();

        if data.info.dst_address == BROADCAST_ADDRESS || !manager.peer_exists(&data.info.src_address) { 
            let mut reply = EspNowSendData{
                data: 0x00_u8.to_be_bytes().to_vec(),
                address: data.info.src_address,
            };
            if !manager.peer_exists(&data.info.src_address) {
                manager
                    .add_peer(PeerInfo {
                        peer_address: data.info.src_address,
                        lmk: None,
                        channel: None,
                        encrypt: false,
                    })
                    .unwrap();
                println!("Added peer {:?}", format_mac!(data.info.src_address));
                reply.data[0] = 0x01;
            }
            // TODO: REGISTER THE NEW DEVICE SOMEWHERE
            esp_now_send_channel.send(reply).await;
            // continue;
        }
        
        let new_data = parse_esp_now_data(data);
        
        if !new_data.is_empty(){
            esp_now_recv_channel.send(new_data).await;
        }
    }
}

#[embassy_executor::task]
async fn net_task(net_stack: &'static embassy_net::Stack<WifiDevice<'static, WifiStaDevice>>) {
    net_stack.run().await
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    println!("start connection task");
    // println!("Device capabilities: {:?}", controller.get_capabilities().unwrap());
    loop {
        match esp_wifi::wifi::get_wifi_state() {
            WifiState::StaConnected => {
                // wait until we're no longer connected
                controller.wait_for_event(WifiEvent::StaDisconnected).await;
                Timer::after(Duration::from_millis(5000)).await
            }
            _ => {}
        }

        let client_config = Configuration::Client(ClientConfiguration {
            ssid: SSID.try_into().unwrap(),
            password: PASSWORD.try_into().unwrap(),
            ..Default::default()
        });
        let conf = controller.get_configuration();

        if conf.is_err() || conf.unwrap().as_client_conf_ref().unwrap().ssid != SSID{
            println!("Setting configuration: {:?}", client_config);
            controller.set_configuration(&client_config).unwrap();
        }
    
        println!("Starting wifi");
        if !matches!(controller.is_started(), Ok(true)) {
            controller.start().await.unwrap();
        }
        println!("Wifi started!");
        
        println!("About to connect...");

        match controller.connect().await {
            Ok(_) => println!("Wifi connected!"),
            Err(e) => {
                println!("Failed to connect to wifi: {e:?}");
                Timer::after(Duration::from_millis(5000)).await
            }
        }
    }
}


#[embassy_executor::task]
async fn wifi_task(
    net_stack: &'static embassy_net::Stack<WifiDevice<'static, WifiStaDevice>>,
    esp_now_send_channel: &'static Channel<CriticalSectionRawMutex, EspNowSendData,5>,
    esp_now_recv_channel: &'static Channel<CriticalSectionRawMutex, Vec<DeviceData>, 5>,
    esp_now_send_status_signal: &'static Signal<CriticalSectionRawMutex, bool>
){
    let mut rx_buffer = [0; 4096];
    let mut tx_buffer = [0; 4096];

    loop {
        if net_stack.is_link_up() {
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    println!("Waiting to get IP address...");
    loop {
        if let Some(config) = net_stack.config_v4() {
            println!("Got IP: {}", config.address);
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    let mut remembered_devices:Vec<DeviceData> = Vec::new();

    loop{
        Timer::after(Duration::from_millis(1_000)).await;

        let mut socket = TcpSocket::new(&net_stack, &mut rx_buffer, &mut tx_buffer);
        // socket.set_timeout(Some(embassy_time::Duration::from_secs(10)));

        let remote_endpoint = (MQTT_ADDRESS, MQTT_PORT);
        println!("Connecting to MQTT server...");
        let connection = socket.connect(remote_endpoint).await;
        if let Err(e) = connection {
            println!("connect error: {:?}", e);
            continue;
        }

        let mut config = ClientConfig::new(
            rust_mqtt::client::client_config::MqttVersion::MQTTv5,
            CountingRng(20000),
        );
        config.add_max_subscribe_qos(QoS0);
        config.add_client_id("esp32-server");
        config.add_username(MQTT_USERNAME);
        config.add_password(MQTT_PASSWORD);
        config.max_packet_size = 100;
        config.keep_alive = 60;

        let mqtt_reset_timeout = config.keep_alive-5;

        let mut recv_buffer = [0; 80];
        let mut write_buffer = [0; 80];

        let mut client =
            MqttClient::<_, 5, _>::new(socket, &mut write_buffer, 80, &mut recv_buffer, 80, config);

        match client.connect_to_broker().await {
            Ok(()) => {}
            Err(mqtt_error) => match mqtt_error {
                ReasonCode::NetworkError => {
                    println!("MQTT Network Error");
                    continue;
                }
                _ => {
                    println!("Other MQTT Error: {:?}", mqtt_error);
                    continue;
                }
            },
        }

        println!("Connected to MQTT server");
        
        // TODO: subscribe to all topics
        let string_topics: Vec<String> = remembered_devices.iter().map(|device| device.command_topic()).collect();
        let topics: heapless::Vec<&str, MAX_MQTT_DEVICES> = string_topics.iter().map(|s| s.as_str()).collect();

        if !topics.is_empty(){
            let res = client.subscribe_to_topics(&topics).await;
            match res {
                Ok(()) => println!("Subscribed to topics: {:?}", topics),
                Err(e) => println!("Error subscribing to topics: {:?}", e),
            }
        }


        let mut ticker = Ticker::every(Duration::from_secs(mqtt_reset_timeout.into()));

        loop{
            let ticker_future = ticker.next();
            let recv_future = client.receive_message();
            let new_devices_future = esp_now_recv_channel.receive();

            let fut_res = select3(ticker_future, recv_future, new_devices_future).await;
            match fut_res {
                Either3::First(_) => {
                    let res = client.send_ping().await;
                    match res {
                        Ok(()) => println!("Pinged MQTT connection"),
                        Err(e) => {
                            println!("Error sending ping: {:?}", e);
                            break;
                        }
                    }
                    continue;
                },
                Either3::Second(result) => {
                    ticker.reset();

                    let msg = match result{
                        Ok(msg) => msg,
                        Err(e) => {
                            println!("Error receiving MQTT message: {:?}", e);
                            break;
                        }
                    };
                    println!("Received MQTT message: {:?}", msg);

                    let device_mac = msg.0.strip_suffix("/set").unwrap();
                    let device_data = match remembered_devices.iter_mut().find(|data| data.command_topic() == device_mac){
                        Some(device_data) => device_data,
                        None => {
                            println!("I can't find such a device in my memory: {:?}", device_mac);
                            continue;
                        }
                    };

                    // let mut device = device.clone();
                    device_data.device.set_json(&String::from_utf8_lossy(msg.1));
                    
                    esp_now_send_channel.send(Into::<EspNowSendData>::into(*device_data)).await;

                    let res = esp_now_send_status_signal.wait().await;
                    if !res {
                        // println!("Error sending esp-now data to device: {:?}", device.device_mac);
                        break;
                    }
                    let topic = device_data.state_topic();
                    let res = client.send_message(topic.as_str(), device_data.device.as_json().as_bytes(), QoS0, false).await;
                    match res {
                        Ok(()) => println!("Sent an MQTT message to topic: {:?}", topic),
                        Err(e) => println!("Error sending an MQTT message to topic: {:?} error code:{:?}", topic, e),
                    }

                }
                Either3::Third(result) => {
                    println!("Received new data: {:?}", result);
                    let new_devices:Vec<DeviceData> = result.iter().filter(
                        |device| remembered_devices.iter().find(|new_device| new_device.mac == device.mac).is_none()
                    ).cloned().collect();
                    let updated_devices:Vec<DeviceData> = result.iter().filter(
                        |device| remembered_devices.iter().find(|new_device| new_device.mac == device.mac).is_some()
                    ).cloned().collect();

                    println!("New devices: {:?}", new_devices);
                    println!("Updated devices: {:?}", updated_devices);

                    let string_topics: Vec<String> = new_devices.iter().map(|device_data| device_data.command_topic()).collect();
                    let topics: heapless::Vec<&str, MAX_MQTT_DEVICES> = string_topics.iter().map(|s| s.as_str()).collect();
                    if !topics.is_empty(){
                        let res = client.subscribe_to_topics(&topics).await;
                        match res {
                            Ok(()) => println!("Subscribed to topics: {:?}", topics),
                            Err(e) => println!("Error subscribing to topics: {:?}", e),
                        }
                    }
                    remembered_devices.extend(new_devices);
                    

                    for device_data in updated_devices{
                        let topic = device_data.state_topic();

                        let res = client.send_message(&topic, device_data.device.as_json().as_bytes(), QoS0, false).await;
                        match res {
                            Ok(()) => println!("Sent a message to topic: {:?}", topic),
                            Err(e) => println!("Error sending a message to topic: {:?} error code:{:?}", topic, e),
                        }
                    }
                }
            }
        }
    }
}

#[main]
async fn main(spawner:Spawner) {
    // esp_println::logger::init_logger(log::LevelFilter::Debug);
    
    init_heap();
    let peripherals = Peripherals::take();
    let system = peripherals.SYSTEM.split();

    let clocks = ClockControl::max(system.clock_control).freeze();

    let rtc = Rtc::new(peripherals.LPWR);
    
    let timer_g1_t0 = TimerGroup::new(peripherals.TIMG1, &clocks).timer0;
    let init = initialize(
        EspWifiInitFor::Wifi,
        timer_g1_t0,
        Rng::new(peripherals.RNG),
        system.radio_clock_control,
        &clocks,
    )
    .unwrap();
    
    println!("My MAC: {:?}",format_mac!(Efuse::get_mac_address()));

    // init embassy
    let timer_g0 = TimerGroup::new(peripherals.TIMG0, &clocks);
    embassy::init(&clocks, timer_g0);

    // init esp_now
    let wifi = peripherals.WIFI;

    let (esp_now_if, esp_now_token) = enable_esp_now_with_wifi(wifi);

    let esp_now = EspNow::new_with_wifi(&init, esp_now_token).unwrap();
    
    let (esp_now_manager, esp_now_sender, esp_now_receiver) = esp_now.split();

    let esp_now_send_channel = &*make_static!(Channel::new());
    let esp_now_recv_channel = &*make_static!(Channel::new());
    let esp_now_send_status_signal = &*make_static!(Signal::new());

    spawner.spawn(esp_now_listener_task(rtc, esp_now_manager, esp_now_receiver, esp_now_send_channel,esp_now_recv_channel)).ok();
    spawner.spawn(esp_now_sender_task(esp_now_sender, esp_now_send_channel, esp_now_send_status_signal)).ok();
    

    let (wifi_interface, wifi_controller) = new_with_mode(&init, esp_now_if, WifiStaDevice).unwrap();
    
    let config = Config::dhcpv4(Default::default());
    let net_stack = make_static!(embassy_net::Stack::new(
        wifi_interface,
        config,
        make_static!(embassy_net::StackResources::<10>::new()),
        1234
    ));
    spawner.spawn(connection(wifi_controller)).ok();
    spawner.spawn(net_task(net_stack)).ok();
    spawner.spawn(wifi_task(net_stack, esp_now_send_channel, esp_now_recv_channel, esp_now_send_status_signal)).ok();
}

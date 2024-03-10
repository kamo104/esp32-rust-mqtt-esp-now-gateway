
extern crate alloc;
use alloc::{
    fmt, format, string::{String, ToString}, vec::Vec,
};

use core::{
    fmt::Debug, iter::Peekable, slice::Iter
};

use log::error;

#[cfg(feature="serde")]
use serde::{Deserialize, Serialize};

#[macro_export]
macro_rules! format_mac {
    ($mac:expr) => {
        format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            $mac[0], $mac[1], $mac[2], $mac[3], $mac[4], $mac[5]
        )
    };
}

#[derive(Debug)]
pub struct EspNowSendData {
    pub data: Vec<u8>,
    pub address: [u8; 6],
}

#[derive(Debug)]
pub enum DeviceError{
    InvalidMessage,
    UnknownDevice,
    UnimplementedType,
}

#[derive(Debug, PartialEq, Clone, Copy)]
#[cfg_attr(feature="serde",derive(Serialize, Deserialize))]
pub struct Color {
    r: u8,
    g: u8,
    b: u8,
}

// based on: https://bthome.io/format/
// CHANGES to the format:
// - everything is big-endian
// - added Light device type (0xFE)
#[repr(u32)]
#[derive(Debug, PartialEq, Clone, Copy)]
#[cfg_attr(feature="serde",derive(Serialize, Deserialize),serde(tag = "type"))]
pub enum EspNowDevice {
    DeviceType(u8) = 0xF0,

    Motion{state:u8} = 0x21,
    Moving{state:u8} = 0x22,
    Light{state:u8,brightness:u8,color:Color} = 0xFE,
}
impl Into<u8> for EspNowDevice{
    fn into(self) -> u8{
        match self{
            EspNowDevice::DeviceType(_) => 0xF0,

            EspNowDevice::Motion{state: _} => 0x21,
            EspNowDevice::Moving{state: _} => 0x22,
            EspNowDevice::Light{color: _, brightness: _, state: _} => 0xFE,
        }
    }
}
impl EspNowDevice{
    pub fn variant(self) -> u8{
        self.into()
    }
    fn try_from(byte:u8) -> Option<EspNowDevice>{
        match byte{
            0xF0 => Some(EspNowDevice::DeviceType(byte)),
            0x21 => Some(EspNowDevice::Motion{state:0}),
            0x22 => Some(EspNowDevice::Moving{state:0}),
            0xFE => Some(EspNowDevice::Light{state:0,brightness:0,color:Color{r:0,g:0,b:0}}),
            _ => None,
        }
    }
    fn as_bytes(self) -> Vec<u8>{
        match self{
            EspNowDevice::DeviceType(byte) => Vec::from([byte]),
            EspNowDevice::Motion{state} => Vec::from([self.variant(), state]),
            EspNowDevice::Moving{state} => Vec::from([self.variant(), state]),
            EspNowDevice::Light{state,brightness,color} => Vec::from([self.variant(), state, brightness, color.r, color.g, color.b]),
        }
    }
    pub fn from_bytes(bytes: &mut Peekable<Iter<u8>>) -> Result<EspNowDevice, DeviceError>{
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
                match bytes.next_chunk::<2>(){
                    Ok(b) => {
                        device = match EspNowDevice::try_from(*b[1]){
                            Some(device) => device,
                            None => return Err(DeviceError::UnknownDevice),
                        };
                    },
                    Err(_non_full_iter) => return Err(DeviceError::InvalidMessage),
                }

                return Ok(device);
            }
            EspNowDevice::Moving{ ref mut state} =>{
                *state = match bytes.next(){
                    Some(b) => *b,
                    None => return Err(DeviceError::InvalidMessage),
                };
                return Ok(device);
            }
            _=>{
                return Err(DeviceError::UnimplementedType);
            }
        }
    }

    #[cfg(feature="serde")]
    pub fn as_json(&self) -> String{
        let mut message:hashbrown::HashMap<String, serde_json::Value> = match serde_json::from_str(serde_json::to_string(self).unwrap().as_str()){
            Ok(message) => message,
            Err(e) => {
                error!("Error deserializing json: {:?}", e);
                return "{}".to_string();
            }
        };
        if let Some(s) = message.remove("state"){
            message.insert("state".to_string(), if s.as_u64().unwrap() == 1 {"ON".into()} else {"OFF".into()});
        }
        
        return serde_json::to_string(&message).unwrap();
    }
    #[cfg(feature="serde")]
    pub fn set_json(&mut self, json: &str){
        let message:hashbrown::HashMap<String, serde_json::Value> = match serde_json::from_str(json){
            Ok(message) => message,
            Err(e) => {
                error!("Error deserializing json: {:?}", e);
                return;
            }
        };
        match self{
            EspNowDevice::Light{state, brightness, color} => {
                if let Some(value) = message.get("state"){
                    *state = match value.as_str(){
                        Some(value) => if value == "ON" {1} else {0},
                        None => {
                            error!("Invalid state value: {:?}", value);
                            return;
                        }
                    };
                }
                if let Some(value) = message.get("brightness"){
                    *brightness = match value.as_u64(){
                        Some(value) => value as u8,
                        None => {
                            error!("Invalid brightness value: {:?}", value);
                            return;
                        }
                    };
                }
                if let Some(value) = message.get("color"){
                    let color_message = match value.as_object() {
                        Some(value) => value,
                        None => {
                            error!("Invalid color value: {:?}", value);
                            return;
                        }
                    };
                    if let Some(value) = color_message.get("r"){
                        color.r = match value.as_u64(){
                            Some(value) => value as u8,
                            None => {
                                error!("Invalid color.r value: {:?}", value);
                                return;
                            }
                        };
                    }
                    if let Some(value) = color_message.get("g"){
                        color.g = match value.as_u64(){
                            Some(value) => value as u8,
                            None => {
                                error!("Invalid color.g value: {:?}", value);
                                return;
                            }
                        };
                    }
                    if let Some(value) = color_message.get("b"){
                        color.b = match value.as_u64(){
                            Some(value) => value as u8,
                            None => {
                                error!("Invalid color.b value: {:?}", value);
                                return;
                            }
                        };
                    }
                }
            }
            _ => {}
        }
    }
}

#[derive(PartialEq,Clone, Copy)]
pub struct MacAddr {
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
pub struct DeviceData {
    pub mac: MacAddr,
    pub device: EspNowDevice,
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
    pub fn state_topic(&self) -> String {
        self.mac.to_string() + "/state"
    }
    pub fn command_topic(&self) -> String {
        self.mac.to_string() + "/set"
    }
}

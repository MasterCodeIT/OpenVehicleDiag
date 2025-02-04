use std::fmt::Display;

use comm_api::ComServerError;
use kwp2000::KWP2000ECU;
use uds::UDSECU;

use self::{kwp2000::read_ecu_identification, uds::read_data};

use super::{
    comm_api::{self, ComServer},
    iface::{Interface, InterfaceConfig, InterfacePayload, InterfaceType, PayloadFlag},
};

pub mod kwp2000;
pub mod obd2;
pub mod uds;
pub mod vin;

#[derive(Debug)]
pub enum ProtocolError {
    CommError(comm_api::ComServerError),
    ProtocolError(Box<dyn CommandError>),
    CustomError(String),
    InvalidResponseSize { expect: usize, actual: usize },
    Timeout,
}

impl ProtocolError {
    pub fn is_timeout(&self) -> bool {
        match &self {
            ProtocolError::CommError(_) => false,
            ProtocolError::ProtocolError(_) => false,
            ProtocolError::CustomError(_) => false,
            ProtocolError::InvalidResponseSize { expect, actual } => false,
            ProtocolError::Timeout => true,
        }
    }
}

impl From<ComServerError> for ProtocolError {
    fn from(x: ComServerError) -> Self {
        ProtocolError::CommError(x)
    }
}

unsafe impl Send for ProtocolError {}
unsafe impl Sync for ProtocolError {}

impl ProtocolError {
    pub fn get_text(&self) -> String {
        match self {
            ProtocolError::CommError(e) => e.to_string(),
            ProtocolError::ProtocolError(e) => e.get_desc(),
            ProtocolError::Timeout => "Communication timeout".into(),
            ProtocolError::CustomError(s) => s.clone(),
            ProtocolError::InvalidResponseSize { expect, actual } => {
                format!("Expected {} bytes, got {} bytes", expect, actual)
            }
        }
    }
}

pub type ProtocolResult<T> = std::result::Result<T, ProtocolError>;

pub trait Selectable: Into<u8> {
    fn get_desc(&self) -> String;
    fn get_name(&self) -> String;
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum CautionLevel {
    // This has no adverse effects on the ECU
    None = 0,
    // This might cause unpredictable behavior
    Warn = 1,
    // Danger Zone - Do not run this unless you know what you are doing!
    Alert = 2,
}

pub trait ECUCommand: Selectable {
    fn get_caution_level(&self) -> CautionLevel;
    fn get_cmd_list() -> Vec<Self>;
}

pub trait CommandError {
    fn get_desc(&self) -> String;
    fn get_help(&self) -> Option<String>;
    fn from_byte(b: u8) -> Self
    where
        Self: Sized;
}

impl std::fmt::Debug for Box<dyn CommandError> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CMDError {}", self.get_desc())
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DTCState {
    None,
    Stored,
    Pending,
    Permanent,
}

#[derive(Debug, Clone)]
pub struct DTC {
    pub(crate) error: String,
    pub(crate) state: DTCState,
    pub(crate) check_engine_on: bool,
    pub(crate) id: u32,
}

impl Display for DTC {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} - State: {:?}, Check engine light on?: {}",
            self.error, self.state, self.check_engine_on
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct DiagCfg {
    pub send_id: u32,
    pub recv_id: u32,
    pub global_id: Option<u32>,
}

#[derive(Debug, Copy, Clone)]
pub enum DiagProtocol {
    KWP2000,
    UDS,
}

#[derive(Debug, Clone)]
pub enum DiagServer {
    KWP2000(KWP2000ECU),
    UDS(UDSECU),
}

impl DiagServer {
    pub fn new(
        protocol: DiagProtocol,
        comm_server: &Box<dyn ComServer>,
        interface_type: InterfaceType,
        interface_cfg: InterfaceConfig,
        tx_flags: Option<Vec<PayloadFlag>>,
        diag_cfg: DiagCfg,
    ) -> ProtocolResult<Self> {
        Ok(match protocol {
            DiagProtocol::KWP2000 => Self::KWP2000(KWP2000ECU::start_diag_session(
                comm_server,
                interface_type,
                interface_cfg,
                tx_flags,
                diag_cfg,
            )?),
            DiagProtocol::UDS => Self::UDS(UDSECU::start_diag_session(
                comm_server,
                interface_type,
                interface_cfg,
                tx_flags,
                diag_cfg,
            )?),
        })
    }

    pub fn get_name<'a>(&self) -> &'a str {
        match self {
            Self::KWP2000(_) => "KWP2000",
            Self::UDS(_) => "UDS",
        }
    }

    pub fn kill_diag_server(&mut self) {
        match self {
            Self::KWP2000(s) => s.exit_diag_session(),
            Self::UDS(s) => s.exit_diag_session(),
        }
    }

    pub fn run_cmd(&mut self, cmd: u8, args: &[u8]) -> ProtocolResult<Vec<u8>> {
        match self {
            Self::KWP2000(s) => s.run_command(cmd, args),
            Self::UDS(s) => s.run_command(cmd, args),
        }
    }

    pub fn into_kwp(&mut self) -> Option<&mut KWP2000ECU> {
        match self {
            Self::KWP2000(s) => Some(s),
            Self::UDS(s) => None,
        }
    }

    pub fn read_errors(&self) -> ProtocolResult<Vec<DTC>> {
        match self {
            Self::KWP2000(s) => s.read_errors(),
            Self::UDS(s) => s.read_errors(),
        }
    }

    pub fn clear_errors(&self) -> ProtocolResult<()> {
        match self {
            Self::KWP2000(s) => s.clear_errors(),
            Self::UDS(s) => s.clear_errors(),
        }
    }

    pub fn get_variant_id(&self) -> ProtocolResult<u32> {
        match self {
            Self::KWP2000(s) => {
                read_ecu_identification::read_dcx_mmc_id(&s).map(|x| x.diag_information as u32)
            }
            Self::UDS(s) => read_data::read_variant_id(s),
        }
    }

    pub fn get_dtc_env_data(&self, dtc: &DTC) -> ProtocolResult<Vec<u8>> {
        match self {
            Self::KWP2000(s) => kwp2000::read_status_dtc::read_status_dtc(s, dtc),
            Self::UDS(s) => Err(ProtocolError::CustomError(
                "Not implemented (get_dtc_env_data)".into(),
            )), // TODO
        }
    }
}

impl Drop for DiagServer {
    fn drop(&mut self) {
        println!("Drop for Diag Server called!");
        self.kill_diag_server()
    }
}

pub trait ProtocolServer: Sized {
    type Command: Selectable + ECUCommand;
    type Error: CommandError + 'static;
    fn start_diag_session(
        comm_server: &Box<dyn ComServer>,
        interface_type: InterfaceType,
        interface_cfg: InterfaceConfig,
        tx_flags: Option<Vec<PayloadFlag>>,
        diag_cfg: DiagCfg,
    ) -> ProtocolResult<Self>;
    fn exit_diag_session(&mut self);
    fn run_command(&self, cmd: u8, args: &[u8]) -> ProtocolResult<Vec<u8>>;
    fn read_errors(&self) -> ProtocolResult<Vec<DTC>>;
    fn is_in_diag_session(&self) -> bool;
    fn get_last_error(&self) -> Option<String>;

    fn run_command_resp(
        interface: &mut Box<dyn Interface>,
        flags: &Option<Vec<PayloadFlag>>,
        send_id: u32,
        cmd: u8,
        args: &[u8],
        receive_require: bool,
    ) -> std::result::Result<Vec<u8>, ProtocolError> {
        let mut tx_data = vec![cmd];
        tx_data.extend_from_slice(args);
        let mut tx = InterfacePayload::new(send_id, &tx_data);
        if let Some(f) = flags {
            tx.flags = f.clone();
        }
        if !receive_require {
            interface
                .send_data(&[tx], 0)
                .map(|_| vec![])
                .map_err(ProtocolError::CommError)
        } else {
            // Await max 1 second for response
            let mut res = interface.send_recv_data(tx, 0, 2000)?;
            if res.data[0] == 0x7F && res.data[2] == 0x78 {
                // ResponsePending
                println!("DIAG - ECU is processing request - Waiting!");
                match interface.recv_data(1, 2000) {
                    Ok(data) => {
                        if let Some(d) = data.get(0) {
                            res = d.clone();
                        } else {
                            return Err(ProtocolError::ProtocolError(Box::new(
                                Self::Error::from_byte(res.data[2]),
                            )));
                        }
                    }
                    Err(e) => return Err(ProtocolError::CommError(e)),
                }
            }
            if res.data[0] == 0x7F {
                // Still error :(
                Err(ProtocolError::ProtocolError(Box::new(
                    Self::Error::from_byte(res.data[2]),
                )))
            } else if res.data[0] == (cmd + 0x40) {
                Ok(res.data)
            } else {
                eprintln!(
                    "DIAG - Command response did not match request? Send: {:02X} - Recv: {:02X}",
                    cmd, res.data[0]
                );
                Err(ProtocolError::Timeout)
            }
        }
    }
}

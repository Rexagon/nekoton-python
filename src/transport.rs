use std::sync::Arc;

use pyo3::prelude::*;

use crate::subscription::{Address, Subscription};
use crate::util::HandleError;

#[derive(Clone)]
#[pyclass(subclass)]
pub struct Transport {
    pub clock: Clock,
    pub handle: TransportHandle,
}

#[pymethods]
impl Transport {
    #[getter]
    pub fn clock(&self) -> Clock {
        self.clock.clone()
    }

    pub fn check_connection<'a>(&self, py: Python<'a>) -> PyResult<&'a PyAny> {
        let handle = self.handle.clone();
        pyo3_asyncio::tokio::future_into_py(py, async move {
            handle.check_local_node_connection().await
        })
    }

    pub fn subscribe<'a>(&self, py: Python<'a>, address: Address) -> PyResult<&'a PyAny> {
        pyo3_asyncio::tokio::future_into_py(py, Subscription::subscribe_impl(self.clone(), address))
    }

    pub fn get_signature_id<'a>(&self, py: Python<'a>) -> PyResult<&'a PyAny> {
        let clock = self.clock.clone();
        let handle = self.handle.clone();
        pyo3_asyncio::tokio::future_into_py(py, async move {
            let capabilities = handle
                .as_ref()
                .get_capabilities(clock.as_ref())
                .await
                .handle_runtime_error()?;
            Ok(capabilities.signature_id())
        })
    }
}

#[derive(Copy, Clone)]
#[pyclass(extends = Transport)]
pub struct GqlTransport;

#[pymethods]
impl GqlTransport {
    #[new]
    fn new(
        endpoints: Vec<String>,
        clock: Option<Clock>,
        local: Option<bool>,
    ) -> PyResult<PyClassInitializer<Self>> {
        use nekoton_transport::gql::*;

        let client = GqlClient::new(GqlNetworkSettings {
            endpoints,
            local: local.unwrap_or_default(),
            ..Default::default()
        })
        .handle_value_error()?;

        let transport = Arc::new(nt::transport::gql::GqlTransport::new(client));
        let handle = TransportHandle::GraphQl(transport);
        let clock = clock.unwrap_or_default();

        Ok(PyClassInitializer::from(Transport { handle, clock }).add_subclass(Self))
    }
}

#[derive(Copy, Clone)]
#[pyclass(extends = Transport)]
pub struct JrpcTransport;

#[pymethods]
impl JrpcTransport {
    #[new]
    fn new(endpoint: &str, clock: Option<Clock>) -> PyResult<PyClassInitializer<Self>> {
        use nekoton_transport::jrpc::JrpcClient;

        let client = JrpcClient::new(endpoint).handle_value_error()?;

        let transport = Arc::new(nt::transport::jrpc::JrpcTransport::new(client));
        let handle = TransportHandle::Jrpc(transport);
        let clock = clock.unwrap_or_default();

        Ok(PyClassInitializer::from(Transport { handle, clock }).add_subclass(Self))
    }
}

#[derive(Default, Clone)]
#[pyclass]
pub struct Clock(pub Arc<nt::utils::ClockWithOffset>);

#[pymethods]
impl Clock {
    /// Creates a new clock with the specified offset in milliseconds.
    #[new]
    pub fn new(offset: Option<i64>) -> Self {
        Self(Arc::new(nt::utils::ClockWithOffset::new(
            offset.unwrap_or_default(),
        )))
    }

    pub fn now_sec(&self) -> u64 {
        nt::utils::Clock::now_sec_u64(self.0.as_ref())
    }

    pub fn now_ms(&self) -> u64 {
        nt::utils::Clock::now_ms_u64(self.0.as_ref())
    }

    #[getter]
    pub fn get_offset(&self) -> i64 {
        self.0.offset_ms()
    }

    #[setter]
    pub fn set_offset(&self, offset: i64) {
        self.0.update_offset(offset)
    }
}

impl<'a> AsRef<dyn nt::utils::Clock + 'a> for Clock {
    fn as_ref(&self) -> &(dyn nt::utils::Clock + 'a) {
        self.0.as_ref()
    }
}

#[derive(Clone)]
pub enum TransportHandle {
    GraphQl(Arc<nt::transport::gql::GqlTransport>),
    Jrpc(Arc<nt::transport::jrpc::JrpcTransport>),
}

impl<'a> AsRef<dyn nt::transport::Transport + 'a> for TransportHandle {
    fn as_ref(&self) -> &(dyn nt::transport::Transport + 'a) {
        match self {
            Self::GraphQl(transport) => transport.as_ref(),
            Self::Jrpc(transport) => transport.as_ref(),
        }
    }
}

impl From<TransportHandle> for Arc<dyn nt::transport::Transport> {
    fn from(handle: TransportHandle) -> Self {
        match handle {
            TransportHandle::GraphQl(transport) => transport,
            TransportHandle::Jrpc(transport) => transport,
        }
    }
}

impl TransportHandle {
    pub async fn check_connection(&self) -> PyResult<()> {
        let transport = self.as_ref();
        if transport.info().has_key_blocks {
            self.check_default_connection().await
        } else {
            self.check_local_node_connection().await
        }
    }

    async fn check_default_connection(&self) -> PyResult<()> {
        self.as_ref()
            .get_contract_state(
                &ton_block::MsgAddressInt::with_standart(None, -1, ton_types::UInt256::ZERO.into())
                    .unwrap(),
            )
            .await
            .handle_runtime_error()?;
        Ok(())
    }

    async fn check_local_node_connection(&self) -> PyResult<()> {
        static GIVER_CODE_HASH: ton_types::UInt256 = ton_types::UInt256::with_array([
            0x4e, 0x92, 0x71, 0x6d, 0xe6, 0x1d, 0x45, 0x6e, 0x58, 0xf1, 0x6e, 0x4e, 0x86, 0x7e,
            0x3e, 0x93, 0xa7, 0x54, 0x83, 0x21, 0xea, 0xce, 0x86, 0x30, 0x1b, 0x51, 0xc8, 0xb8,
            0x0c, 0xa6, 0x23, 0x9b,
        ]);

        self.as_ref()
            .get_accounts_by_code_hash(&GIVER_CODE_HASH, 1, &None)
            .await
            .handle_runtime_error()?;
        Ok(())
    }
}

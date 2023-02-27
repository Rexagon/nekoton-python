use std::sync::{Arc, Weak};
use std::time::Duration;

use nt::core::models;
use pyo3::exceptions::{PyRuntimeError, PyStopAsyncIteration, PyValueError};
use pyo3::prelude::*;
use tokio::sync::{oneshot, watch, Notify};

use crate::abi::SignedExternalMessage;
use crate::models::{AccountState, Address, BlockchainConfig, Message, Transaction};
use crate::util::*;

#[pyclass(subclass)]
pub struct Transport(Arc<TransportState>);

struct TransportState {
    clock: Clock,
    handle: TransportHandle,
    subscriptions: Arc<tokio::sync::Mutex<SubscriptionsMap>>,
}

impl TransportState {
    async fn get_subscription(
        &self,
        address: ton_block::MsgAddressInt,
    ) -> PyResult<Arc<SharedSubscription>> {
        use std::collections::hash_map;

        let mut subscriptions = self.subscriptions.lock().await;
        let subscription = match subscriptions.entry(address.clone()) {
            hash_map::Entry::Occupied(mut entry) => {
                if let Some(subscription) = entry.get().upgrade() {
                    subscription
                } else {
                    let subscription = SharedSubscription::subscribe(
                        self.clock.clone(),
                        self.handle.clone(),
                        address,
                    )
                    .await
                    .handle_runtime_error()?;
                    entry.insert(Arc::downgrade(&subscription));
                    subscription
                }
            }
            hash_map::Entry::Vacant(entry) => {
                let subscription =
                    SharedSubscription::subscribe(self.clock.clone(), self.handle.clone(), address)
                        .await
                        .handle_runtime_error()?;
                entry.insert(Arc::downgrade(&subscription));
                subscription
            }
        };

        Ok(subscription)
    }
}

type SubscriptionsMap = FastHashMap<ton_block::MsgAddressInt, Weak<SharedSubscription>>;

#[pymethods]
impl Transport {
    #[getter]
    pub fn clock(&self) -> Clock {
        self.0.clock.clone()
    }

    pub fn check_connection<'a>(&self, py: Python<'a>) -> PyResult<&'a PyAny> {
        let handle = self.0.handle.clone();
        pyo3_asyncio::tokio::future_into_py(py, async move {
            handle.check_local_node_connection().await
        })
    }

    pub fn send_external_message<'a>(
        &self,
        py: Python<'a>,
        message: PyRef<'a, SignedExternalMessage>,
    ) -> PyResult<&'a PyAny> {
        let expire_at = message.expire_at;
        let message = message.into_super().clone();

        let dst = {
            let ton_block::CommonMsgInfo::ExtInMsgInfo(info) = message.data.header() else {
                return Err(PyValueError::new_err("Expected external outbound message"));
            };
            info.dst.clone()
        };

        let shared = self.0.clone();
        pyo3_asyncio::tokio::future_into_py(py, async move {
            let subscription = shared.get_subscription(dst).await?;

            // TODO: add watchdog timer to delay subscription drop

            subscription.send_message(&message, expire_at).await
        })
    }

    pub fn get_signature_id<'a>(&self, py: Python<'a>) -> PyResult<&'a PyAny> {
        let state = self.0.clone();
        pyo3_asyncio::tokio::future_into_py(py, async move {
            let capabilities = state
                .handle
                .as_ref()
                .get_capabilities(state.clock.as_ref())
                .await
                .handle_runtime_error()?;
            Ok(capabilities.signature_id())
        })
    }

    pub fn get_blockchain_config<'a>(
        &self,
        py: Python<'a>,
        force: Option<bool>,
    ) -> PyResult<&'a PyAny> {
        let force = force.unwrap_or_default();
        let state = self.0.clone();
        pyo3_asyncio::tokio::future_into_py(py, async move {
            let config = state
                .handle
                .as_ref()
                .get_blockchain_config(state.clock.as_ref(), force)
                .await
                .handle_runtime_error()?;

            Ok(BlockchainConfig(Arc::new(config)))
        })
    }

    pub fn get_account_state<'a>(&self, py: Python<'a>, address: Address) -> PyResult<&'a PyAny> {
        let handle = self.0.handle.clone();
        pyo3_asyncio::tokio::future_into_py(py, async move {
            let state = handle
                .as_ref()
                .get_contract_state(&address.0)
                .await
                .handle_runtime_error()?;

            Ok(match state {
                nt::transport::models::RawContractState::NotExists => None,
                nt::transport::models::RawContractState::Exists(state) => {
                    Some(AccountState(state.account))
                }
            })
        })
    }

    pub fn get_accounts_by_code_hash<'a>(
        &self,
        py: Python<'a>,
        code_hash: &[u8],
        continuation: Option<Address>,
        limit: Option<u8>,
    ) -> PyResult<&'a PyAny> {
        const DEFAULT_LIMIT: u8 = 50;

        let code_hash = ton_types::UInt256::from_bytes(code_hash, "code hash")?;

        let handle = self.0.handle.clone();
        pyo3_asyncio::tokio::future_into_py(py, async move {
            let addresses = handle
                .as_ref()
                .get_accounts_by_code_hash(
                    &code_hash,
                    limit.unwrap_or(DEFAULT_LIMIT),
                    &continuation.map(|Address(addr)| addr),
                )
                .await
                .handle_runtime_error()?;

            Ok(addresses.into_iter().map(Address).collect::<Vec<_>>())
        })
    }

    pub fn get_transaction<'a>(
        &self,
        py: Python<'a>,
        transaction_hash: &[u8],
    ) -> PyResult<&'a PyAny> {
        let transaction_hash =
            ton_types::UInt256::from_bytes(transaction_hash, "transaction hash")?;

        let handle = self.0.handle.clone();
        pyo3_asyncio::tokio::future_into_py(py, async move {
            match handle
                .as_ref()
                .get_transaction(&transaction_hash)
                .await
                .handle_runtime_error()?
            {
                None => Ok(None),
                Some(tx) => Transaction::try_from(tx).map(Some),
            }
        })
    }

    pub fn get_dst_transaction<'a>(
        &self,
        py: Python<'a>,
        message_hash: &[u8],
    ) -> PyResult<&'a PyAny> {
        let message_hash = ton_types::UInt256::from_bytes(message_hash, "message hash")?;

        let handle = self.0.handle.clone();
        pyo3_asyncio::tokio::future_into_py(py, async move {
            match handle
                .as_ref()
                .get_dst_transaction(&message_hash)
                .await
                .handle_runtime_error()?
            {
                None => Ok(None),
                Some(tx) => Transaction::try_from(tx).map(Some),
            }
        })
    }

    pub fn get_transactions<'a>(
        &self,
        py: Python<'a>,
        address: Address,
        lt: Option<u64>,
        limit: Option<u8>,
    ) -> PyResult<&'a PyAny> {
        const DEFAULT_LIMIT: u8 = 50;

        let handle = self.0.handle.clone();
        pyo3_asyncio::tokio::future_into_py(py, async move {
            let raw_transactions = handle
                .as_ref()
                .get_transactions(
                    &address.0,
                    lt.unwrap_or(u64::MAX),
                    limit.unwrap_or(DEFAULT_LIMIT),
                )
                .await
                .handle_runtime_error()?;

            raw_transactions
                .into_iter()
                .map(Transaction::try_from)
                .collect::<PyResult<Vec<_>>>()
        })
    }

    pub fn account_states(&self, address: Address) -> AccountStatesAsyncIter {
        AccountStatesAsyncIter(Arc::new(tokio::sync::Mutex::new(
            AccountStatesAsyncIterState::Uninit {
                transport: self.0.clone(),
                address: address.0,
            },
        )))
    }
}

#[derive(Copy, Clone)]
#[pyclass(subclass, extends = Transport)]
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

        Ok(PyClassInitializer::from(Transport(Arc::new(TransportState {
            handle,
            clock,
            subscriptions: Default::default(),
        })))
        .add_subclass(Self))
    }
}

#[derive(Copy, Clone)]
#[pyclass(subclass, extends = Transport)]
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

        Ok(PyClassInitializer::from(Transport(Arc::new(TransportState {
            handle,
            clock,
            subscriptions: Default::default(),
        })))
        .add_subclass(Self))
    }
}

#[pyclass]
pub struct AccountStatesAsyncIter(Arc<tokio::sync::Mutex<AccountStatesAsyncIterState>>);

enum AccountStatesAsyncIterState {
    Uninit {
        transport: Arc<TransportState>,
        address: ton_block::MsgAddressInt,
    },
    Active {
        watch: watch::Receiver<PyObject>,
        initial: bool,
        _subscription: Arc<SharedSubscription>,
    },
    Closed,
}

#[pymethods]
impl AccountStatesAsyncIter {
    fn close<'a>(&self, py: Python<'a>) -> PyResult<&'a PyAny> {
        let state = self.0.clone();
        pyo3_asyncio::tokio::future_into_py(py, async move {
            *state.lock().await = AccountStatesAsyncIterState::Closed;
            Ok(())
        })
    }

    fn __aenter__<'a>(slf: PyRef<'a, Self>, py: Python<'a>) -> PyResult<&'a PyAny> {
        let state = slf.0.clone();
        let slf = slf.into_py(py);
        pyo3_asyncio::tokio::future_into_py(py, async move {
            let mut state = state.lock().await;
            match &*state {
                AccountStatesAsyncIterState::Active { .. } => Ok(slf),
                AccountStatesAsyncIterState::Closed => Err(PyRuntimeError::new_err(
                    "Entering closed states subscription",
                )),
                AccountStatesAsyncIterState::Uninit { transport, address } => {
                    let subscription = transport.get_subscription(address.clone()).await?;
                    let watch = subscription.state.account_state.subscribe();
                    *state = AccountStatesAsyncIterState::Active {
                        watch,
                        initial: true,
                        _subscription: subscription,
                    };
                    Ok(slf)
                }
            }
        })
    }

    fn __aexit__<'a>(
        &self,
        py: Python<'a>,
        _exc_type: &'a PyAny,
        _exc_value: &'a PyAny,
        _traceback: &'a PyAny,
    ) -> PyResult<&'a PyAny> {
        self.close(py)
    }

    fn __aiter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    pub fn __anext__<'a>(&'a mut self, py: Python<'a>) -> PyResult<Option<&'a PyAny>> {
        let state = self.0.clone();
        match pyo3_asyncio::tokio::future_into_py(py, async move {
            let mut state = state.lock().await;
            match &*state {
                AccountStatesAsyncIterState::Closed => {
                    return Err(PyStopAsyncIteration::new_err(()))
                }
                AccountStatesAsyncIterState::Uninit { transport, address } => {
                    let subscription = transport.get_subscription(address.clone()).await?;
                    let watch = subscription.state.account_state.subscribe();
                    *state = AccountStatesAsyncIterState::Active {
                        watch,
                        initial: true,
                        _subscription: subscription,
                    };
                }
                _ => {}
            };

            match &mut *state {
                AccountStatesAsyncIterState::Active { watch, initial, .. } => {
                    if std::mem::take(initial) {
                        return Ok(watch.borrow_and_update().clone());
                    }

                    if watch.changed().await.is_err() {
                        return Err(PyStopAsyncIteration::new_err(()));
                    }

                    Ok(watch.borrow_and_update().clone())
                }
                _ => unreachable!(),
            }
        }) {
            Ok(awaitable) => Ok(Some(awaitable)),
            Err(e) => Err(e),
        }
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

    #[getter]
    pub fn now_sec(&self) -> u64 {
        nt::utils::Clock::now_sec_u64(self.0.as_ref())
    }

    #[getter]
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

    fn __repr__(&self) -> String {
        format!("Clock(offset={})", self.get_offset())
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

struct SharedSubscription {
    state: SubscriptionState,
    skip_iteration_signal: Arc<Notify>,
    subscription: tokio::sync::Mutex<nt::core::ContractSubscription>,
}

impl SharedSubscription {
    async fn subscribe(
        clock: Clock,
        transport: TransportHandle,
        address: ton_block::MsgAddressInt,
    ) -> PyResult<Arc<Self>> {
        let (account_state, _) = watch::channel(py_none());

        let state = SubscriptionState {
            account_state,
            pending_messages: Default::default(),
        };

        let subscription = tokio::sync::Mutex::new(
            nt::core::ContractSubscription::subscribe(
                clock.0,
                transport.into(),
                address,
                &mut |account_state| state.on_state_changed(account_state.clone()),
                None,
            )
            .await
            .handle_runtime_error()?,
        );

        let shared = Arc::new(SharedSubscription {
            state,
            skip_iteration_signal: Arc::new(Default::default()),
            subscription,
        });

        tokio::spawn(subscription_loop(shared.clone()));

        Ok(shared)
    }

    async fn send_message(
        &self,
        message: &Message,
        expire_at: u32,
    ) -> PyResult<Option<Transaction>> {
        use dashmap::mapref::entry;

        let (tx, rx) = oneshot::channel();
        match self.state.pending_messages.entry(message.hash) {
            entry::Entry::Occupied(_) => return Err(PyRuntimeError::new_err("Duplicate message")),
            entry::Entry::Vacant(entry) => {
                entry.insert(tx);
            }
        }

        let pending_message = {
            let mut subscription = self.subscription.lock().await;
            subscription.send(&message.data, expire_at).await
        };

        match pending_message {
            Ok(tx) => {
                if tx.message_hash != message.hash {
                    // TODO: panic instead?
                    self.state.pending_messages.remove(&message.hash);
                    return Err(PyRuntimeError::new_err("Pending message mismatch"));
                }
            }
            Err(e) => {
                self.state.pending_messages.remove(&message.hash);
                return Err(e).handle_runtime_error();
            }
        }

        let result = rx.await.handle_runtime_error();
        self.state.pending_messages.remove(&message.hash);

        match result.handle_runtime_error()? {
            ReceivedTransaction::Expired => Ok(None),
            ReceivedTransaction::Invalid => {
                // TODO: panic instead?
                Err(PyRuntimeError::new_err("Failed to parse transaction"))
            }
            ReceivedTransaction::Valid(tx) => Ok(Some(tx)),
        }
    }
}

async fn subscription_loop(shared: Arc<SharedSubscription>) {
    fn split_shared(shared: Arc<SharedSubscription>) -> (Arc<Notify>, Weak<SharedSubscription>) {
        (
            shared.skip_iteration_signal.clone(),
            Arc::downgrade(&shared),
        )
    }

    const INTERVAL: Duration = Duration::from_secs(5);
    const SHORT_INTERVAL: Duration = Duration::from_secs(1);

    let (skip_iteration_signal, shared) = split_shared(shared);

    let mut polling_method = models::PollingMethod::Manual;
    loop {
        let interval = match polling_method {
            models::PollingMethod::Manual => INTERVAL,
            models::PollingMethod::Reliable => SHORT_INTERVAL,
        };

        let signal = skip_iteration_signal.notified();
        tokio::select! {
            _ = signal => {},
            _ = tokio::time::sleep(interval) => {}
        }

        let Some(shared) = shared.upgrade() else {
            return;
        };

        // TODO: add support for block traversal

        let mut subscription = shared.subscription.lock().await;
        let res = subscription
            .refresh(
                &mut |state| shared.state.on_state_changed(state.clone()),
                &mut |_transactions, _batch_info| {
                    // TODO: handle transactions
                },
                &mut |pending_transaction, transaction| {
                    shared
                        .state
                        .on_message_sent(pending_transaction, transaction)
                },
                &mut |pending_transaction| shared.state.on_message_expired(pending_transaction),
            )
            .await;

        if let Err(e) = res {
            log::error!("Subscription loop error: {e:?}");
        }

        polling_method = subscription.polling_method();
    }
}

struct SubscriptionState {
    account_state: watch::Sender<PyObject>,
    pending_messages: FastDashMap<ton_types::UInt256, ResultTx>,
}

impl SubscriptionState {
    fn on_state_changed(&self, new_state: nt::transport::models::RawContractState) {
        let value = Python::with_gil(|py| match new_state.into_account() {
            ton_block::Account::AccountNone => py.None(),
            ton_block::Account::Account(stuff) => AccountState(stuff).into_py(py),
        });
        self.account_state.send_replace(value);
    }

    fn on_message_sent(
        &self,
        pending_transaction: models::PendingTransaction,
        transaction: nt::transport::models::RawTransaction,
    ) {
        if let Some((_, tx)) = self
            .pending_messages
            .remove(&pending_transaction.message_hash)
        {
            _ = tx.send(match Transaction::try_from(transaction) {
                Ok(transaction) => ReceivedTransaction::Valid(transaction),
                Err(_) => ReceivedTransaction::Invalid,
            });
        }
    }

    fn on_message_expired(&self, pending_transaction: models::PendingTransaction) {
        if let Some((_, tx)) = self
            .pending_messages
            .remove(&pending_transaction.message_hash)
        {
            _ = tx.send(ReceivedTransaction::Expired);
        }
    }
}

enum ReceivedTransaction {
    Expired,
    Invalid,
    Valid(Transaction),
}

type ResultTx = oneshot::Sender<ReceivedTransaction>;

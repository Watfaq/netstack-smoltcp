use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use smoltcp::{
    phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken},
    time::Instant,
};
use tokio::sync::mpsc::{unbounded_channel, Permit, Sender, UnboundedReceiver, UnboundedSender};

use super::packet::AnyIpPktFrame;

pub(super) struct VirtualDevice {
    in_buf_avail: Arc<AtomicBool>,
    in_buf: UnboundedReceiver<Vec<u8>>,
    out_buf: Sender<AnyIpPktFrame>,
}

impl VirtualDevice {
    pub(super) fn new(
        iface_egress_tx: Sender<AnyIpPktFrame>,
    ) -> (Self, UnboundedSender<Vec<u8>>, Arc<AtomicBool>) {
        let iface_ingress_tx_avail = Arc::new(AtomicBool::new(false));
        let (iface_ingress_tx, iface_ingress_rx) = unbounded_channel();
        (
            Self {
                in_buf_avail: iface_ingress_tx_avail.clone(),
                in_buf: iface_ingress_rx,
                out_buf: iface_egress_tx,
            },
            iface_ingress_tx,
            iface_ingress_tx_avail,
        )
    }
}

impl Device for VirtualDevice {
    type RxToken<'a> = VirtualRxToken;
    type TxToken<'a> = VirtualTxToken<'a>;

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let Ok(buffer) = self.in_buf.try_recv() else {
            self.in_buf_avail.store(false, Ordering::Release);
            return None;
        };

        let Ok(permit) = self.out_buf.try_reserve() else {
            self.in_buf_avail.store(false, Ordering::Release);
            return None;
        };

        Some((Self::RxToken { buffer }, Self::TxToken { permit }))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        match self.out_buf.try_reserve() {
            Ok(permit) => Some(Self::TxToken { permit }),
            Err(_) => None,
        }
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut capabilities = DeviceCapabilities::default();
        capabilities.medium = Medium::Ip;
        capabilities.max_transmission_unit = 1504;
        #[cfg(feature = "offload")]
        {
            use smoltcp::phy::{Checksum, ChecksumCapabilities};
            capabilities.checksum = ChecksumCapabilities::ignored();
            // for udp, the tx checksum is required, since the egress tcp packet will be checked by the system stack
            capabilities.checksum.tcp = Checksum::Tx;
            // i don't know why exactly the udp checksum can be ignored, but according to my test, it just work
            capabilities.checksum.udp = Checksum::None;
            capabilities.checksum.ipv4 = Checksum::Tx;
        }
        capabilities
    }
}

pub(super) struct VirtualRxToken {
    buffer: Vec<u8>,
}

impl RxToken for VirtualRxToken {
    fn consume<R, F>(mut self, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        f(&mut self.buffer[..])
    }
}

pub(super) struct VirtualTxToken<'a> {
    permit: Permit<'a, Vec<u8>>,
}

impl<'a> TxToken for VirtualTxToken<'a> {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buffer = vec![0u8; len];
        let result = f(&mut buffer);
        self.permit.send(buffer);
        result
    }
}

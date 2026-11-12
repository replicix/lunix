//! The contract macros compile against trivial implementations and the
//! implementations satisfy them.

mod trivial;

use trivial::{
    index::IFix, journal::JFix, map::MFix, overlay::TOverlay, space::TSpace, store::SFix,
};

lunix_iface::contract::journal!(JFix::new);
lunix_iface::contract::overlay!(TOverlay::new);
lunix_iface::contract::map!(MFix::new);
lunix_iface::contract::index!(IFix::new);
lunix_iface::contract::store!(SFix::new);
lunix_iface::contract::space!(TSpace::new);

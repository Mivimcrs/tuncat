//! ICS pulse: the core repair trick, ported from the verified PowerShell
//! script to native COM late-binding calls against `HNetCfg.HNetShare`.
//!
//! The ICS interfaces (netcon.h) are not exposed by the `windows` crate, so
//! this module talks to them through `IDispatch` — exactly what PowerShell
//! does under the hood. Method names are resolved at runtime via
//! `GetIDsOfNames`.

use anyhow::{bail, Context, Result};
use std::thread::sleep;
use std::time::Duration;
use tracing::{info, warn};

use windows::core::{Interface, GUID, PCWSTR};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, IDispatch, CLSCTX_ALL,
    COINIT_APARTMENTTHREADED, DISPATCH_FLAGS, DISPPARAMS,
};
use windows::Win32::System::Ole::IEnumVARIANT;
use windows::Win32::System::Variant::VARENUM;
use windows::Win32::System::Variant::{
    VariantClear, VARIANT, VT_BOOL, VT_BSTR, VT_DISPATCH, VT_I4, VT_UNKNOWN,
};

use crate::config::PulseDirection;

const PROG_ID: &str = "HNetCfg.HNetShare";
const LOCALE_USER_DEFAULT: u32 = 0x0400;
const DISPID_NEWENUM: i32 = -4;

/// Which side of the sharing relationship an adapter was on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareRole {
    Public,
    Private,
}

impl ShareRole {
    fn as_i4(self) -> i32 {
        match self {
            ShareRole::Public => 0,  // ICSSHARINGTYPE_PUBLIC
            ShareRole::Private => 1, // ICSSHARINGTYPE_PRIVATE
        }
    }

    fn from_i4(v: i32) -> Option<Self> {
        match v {
            0 => Some(ShareRole::Public),
            1 => Some(ShareRole::Private),
            _ => None,
        }
    }
}

// ---------- low-level IDispatch helpers ----------

/// Resolve a method/property name to its DISPID.
fn dispid(disp: &IDispatch, name: &str) -> Result<i32> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let names = [PCWSTR(wide.as_ptr())];
    let mut id = 0i32;
    unsafe {
        disp.GetIDsOfNames(
            &GUID::zeroed(),
            names.as_ptr(),
            1,
            LOCALE_USER_DEFAULT,
            &mut id,
        )
    }
    .with_context(|| format!("GetIDsOfNames('{name}') failed (no such member?)"))?;
    Ok(id)
}

/// Invoke by DISPID. `args` are in logical (declaration) order; the COM
/// reversed-argument convention is handled here.
fn invoke_dispid(
    disp: &IDispatch,
    id: i32,
    flags: DISPATCH_FLAGS,
    args: &[VARIANT],
) -> Result<VARIANT> {
    // Reversed per DISPPARAMS convention.
    let mut rev: Vec<VARIANT> = args.iter().rev().cloned().collect();
    let arg_ptr: *mut VARIANT = rev.as_mut_ptr();
    let params = DISPPARAMS {
        rgvarg: if rev.is_empty() {
            std::ptr::null_mut()
        } else {
            arg_ptr
        },
        rgdispidNamedArgs: std::ptr::null_mut(),
        cArgs: rev.len() as u32,
        cNamedArgs: 0,
    };
    let mut result = VARIANT::default();
    let invoke_result = unsafe {
        disp.Invoke(
            id,
            &GUID::zeroed(),
            LOCALE_USER_DEFAULT,
            flags,
            &params,
            Some(&mut result),
            None,
            None,
        )
    };
    // Free the argument copies (they hold interface/bstr references).
    for v in rev.iter() {
        unsafe { VariantClear(v as *const VARIANT as *mut VARIANT) }.ok();
    }
    invoke_result.with_context(|| format!("Invoke(dispid={id}) failed"))?;
    Ok(result)
}

/// Call a method by name with arguments.
fn call(disp: &IDispatch, name: &str, args: &[VARIANT]) -> Result<VARIANT> {
    let id = dispid(disp, name)?;
    invoke_dispid(disp, id, DISPATCH_FLAGS(1), args)
}

/// Read a property by name.
fn get(disp: &IDispatch, name: &str) -> Result<VARIANT> {
    let id = dispid(disp, name)?;
    invoke_dispid(disp, id, DISPATCH_FLAGS(2), &[])
}

// ---------- VARIANT constructors / extractors ----------

fn var_i4(v: i32) -> VARIANT {
    let mut var = VARIANT::default();
    unsafe {
        (*var.Anonymous.Anonymous).vt = VT_I4;
        (*var.Anonymous.Anonymous).Anonymous.lVal = v;
    }
    var
}

fn var_dispatch(d: &IDispatch) -> VARIANT {
    let mut var = VARIANT::default();
    unsafe {
        (*var.Anonymous.Anonymous).vt = VT_DISPATCH;
        (*var.Anonymous.Anonymous).Anonymous.pdispVal =
            std::mem::ManuallyDrop::new(Some(d.clone()));
    }
    var
}

fn var_vt(var: &VARIANT) -> VARENUM {
    unsafe { (*var.Anonymous.Anonymous).vt }
}

/// Extract a String (frees the BSTR).
fn var_take_string(var: &mut VARIANT) -> Option<String> {
    if var_vt(var) != VT_BSTR {
        return None;
    }
    unsafe {
        let bstr = std::mem::ManuallyDrop::take(&mut (*var.Anonymous.Anonymous).Anonymous.bstrVal);
        let s: Option<String> = Some(bstr.to_string());
        if !bstr.is_empty() {
            windows::Win32::Foundation::SysFreeString(&bstr);
        }
        s
    }
}

/// Extract an IDispatch (add-ref'd clone, clears the variant).
fn var_take_dispatch(var: &mut VARIANT) -> Option<IDispatch> {
    if var_vt(var) != VT_DISPATCH {
        return None;
    }
    unsafe {
        let d = std::mem::ManuallyDrop::take(&mut (*var.Anonymous.Anonymous).Anonymous.pdispVal);
        // Leave VT_EMPTY so VariantClear won't release again.
        (*var.Anonymous.Anonymous).vt = VARENUM(0);
        d
    }
}

fn var_to_bool(var: &VARIANT) -> Option<bool> {
    if var_vt(var) != VT_BOOL {
        return None;
    }
    unsafe { Some((*var.Anonymous.Anonymous).Anonymous.boolVal.0 != 0) }
}

fn var_to_i4(var: &VARIANT) -> Option<i32> {
    if var_vt(var) != VT_I4 {
        return None;
    }
    unsafe { Some((*var.Anonymous.Anonymous).Anonymous.lVal) }
}

fn var_take_unknown(var: &mut VARIANT) -> Option<windows::core::IUnknown> {
    if var_vt(var) != VT_UNKNOWN {
        return None;
    }
    unsafe {
        let u = std::mem::ManuallyDrop::take(&mut (*var.Anonymous.Anonymous).Anonymous.punkVal);
        (*var.Anonymous.Anonymous).vt = VARENUM(0);
        u
    }
}

// ---------- high-level ICS model ----------

/// One ICS-manageable connection with live dispatch handles.
#[allow(dead_code)]
pub struct IcsConnection {
    pub name: String,
    pub device_name: String,
    pub sharing_enabled: bool,
    pub sharing_role: Option<ShareRole>,
    conn: IDispatch,
    cfg: IDispatch,
}

impl IcsConnection {
    pub fn disable_sharing(&self) -> Result<()> {
        call(&self.cfg, "DisableSharing", &[])
            .map(|_| ())
            .with_context(|| format!("DisableSharing failed on '{}'", self.name))
    }

    pub fn enable_sharing(&self, role: ShareRole) -> Result<()> {
        call(&self.cfg, "EnableSharing", &[var_i4(role.as_i4())])
            .map(|_| ())
            .with_context(|| format!("EnableSharing({role:?}) failed on '{}'", self.name))
    }
}

/// Owns the COM apartment for the calling thread. Create one per pulse
/// sequence; COM objects must not outlive it.
pub struct IcsPulser {
    manager: IDispatch,
}

impl IcsPulser {
    /// Initialize COM on this thread (STA) and create the sharing manager.
    pub fn new() -> Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }
            .ok()
            .context("CoInitializeEx failed")?;

        let clsid = unsafe { clsid_from_progid() }
            .context("failed to resolve HNetCfg.HNetShare CLSID (ICS not installed?)")?;
        let manager: IDispatch = unsafe { CoCreateInstance(&clsid, None, CLSCTX_ALL) }
            .context("failed to create HNetCfg.HNetShare manager")?;
        Ok(Self { manager })
    }

    /// Enumerate every connection with its current sharing state.
    /// Handles go stale after Enable/DisableSharing — always re-enumerate.
    pub fn list_connections(&self) -> Result<Vec<IcsConnection>> {
        let mut coll_var = call(&self.manager, "EnumEveryConnection", &[])
            .context("EnumEveryConnection failed")?;
        let coll = var_take_dispatch(&mut coll_var)
            .context("EnumEveryConnection returned a non-object value")?;

        let mut out = Vec::new();
        for conn in enum_collection(&coll)? {
            let Ok(mut props_var) =
                call(&self.manager, "NetConnectionProps", &[var_dispatch(&conn)])
            else {
                warn!("NetConnectionProps failed for one connection; skipping");
                continue;
            };
            let Some(props) = var_take_dispatch(&mut props_var) else {
                continue;
            };

            let name = get(&props, "Name")
                .ok()
                .and_then(|mut v| var_take_string(&mut v))
                .unwrap_or_default();
            let device_name = get(&props, "DeviceName")
                .ok()
                .and_then(|mut v| var_take_string(&mut v))
                .unwrap_or_default();

            let Ok(mut cfg_var) = call(
                &self.manager,
                "INetSharingConfigurationForINetConnection",
                &[var_dispatch(&conn)],
            ) else {
                warn!("no ICS config for connection '{name}'; skipping");
                continue;
            };
            let Some(cfg) = var_take_dispatch(&mut cfg_var) else {
                continue;
            };

            let sharing_enabled = get(&cfg, "SharingEnabled")
                .ok()
                .and_then(|v| var_to_bool(&v))
                .unwrap_or(false);
            let sharing_role = get(&cfg, "SharingConnectionType")
                .ok()
                .and_then(|v| var_to_i4(&v))
                .and_then(ShareRole::from_i4);

            out.push(IcsConnection {
                name,
                device_name,
                sharing_enabled,
                sharing_role: if sharing_enabled { sharing_role } else { None },
                conn,
                cfg,
            });
        }
        Ok(out)
    }

    /// Find a connection by exact name first, then by substring.
    fn find(&self, conns: &[IcsConnection], target: &str) -> Option<usize> {
        conns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(target))
            .or_else(|| {
                conns
                    .iter()
                    .position(|c| c.name.contains(target) || c.device_name.contains(target))
            })
    }

    /// Execute the full ICS pulse. Never leaves sharing enabled on error —
    /// the disable/restore steps run in a `finally`-style tail.
    pub fn pulse(
        &self,
        tun_name: &str,
        public_name: &str,
        direction: PulseDirection,
        hold_sec: u64,
        restore: bool,
    ) -> Result<PulseReport> {
        let mut report = PulseReport::default();

        // 1. Snapshot current sharing so we can restore it afterwards.
        let conns = self.list_connections()?;
        let snapshot: Vec<(String, ShareRole)> = conns
            .iter()
            .filter(|c| c.sharing_enabled)
            .filter_map(|c| c.sharing_role.map(|r| (c.name.clone(), r)))
            .collect();
        report.preexisting_sharing = snapshot.iter().map(|(n, _)| n.clone()).collect();
        info!(
            "ICS pulse start: {} preexisting sharing entries",
            snapshot.len()
        );

        // 2. Disable all current sharing.
        self.disable_all(&conns, &mut report);
        sleep(Duration::from_secs(1));

        // 3. Re-enable: private side first, then public side. Re-enumerate
        //    because handles went stale.
        let main = self.do_enable(tun_name, public_name, direction, hold_sec, &mut report);

        // 4. FINALLY: disable everything again, then restore the snapshot.
        let cleanup = (|| -> Result<()> {
            let conns = self.list_connections()?;
            self.disable_all(&conns, &mut report);
            if restore && !snapshot.is_empty() {
                sleep(Duration::from_millis(500));
                let conns = self.list_connections()?;
                for (name, role) in &snapshot {
                    if let Some(idx) = self.find(&conns, name) {
                        if let Err(e) = conns[idx].enable_sharing(*role) {
                            warn!("failed to restore sharing on '{}': {}", name, e);
                            report.restore_failures.push(name.clone());
                        } else {
                            info!("restored sharing on '{}' ({:?})", name, role);
                        }
                    }
                }
            }
            Ok(())
        })();

        // Surface errors from the main sequence first.
        main.context("ICS pulse failed during enable phase")?;
        cleanup.context("ICS pulse cleanup failed")?;
        info!("ICS pulse finished");
        Ok(report)
    }

    fn do_enable(
        &self,
        tun_name: &str,
        public_name: &str,
        direction: PulseDirection,
        hold_sec: u64,
        report: &mut PulseReport,
    ) -> Result<()> {
        let conns = self.list_connections()?;
        let tun_idx = self
            .find(&conns, tun_name)
            .with_context(|| format!("TUN adapter '{tun_name}' not visible to ICS"))?;
        let pub_idx = self
            .find(&conns, public_name)
            .with_context(|| format!("public adapter '{public_name}' not visible to ICS"))?;
        if tun_idx == pub_idx {
            bail!(
                "TUN and public adapter resolved to the same connection '{}'",
                conns[tun_idx].name
            );
        }
        report.tun_adapter = conns[tun_idx].name.clone();
        report.public_adapter = conns[pub_idx].name.clone();
        info!(
            "pulse adapters: tun='{}', public='{}'",
            report.tun_adapter, report.public_adapter
        );

        let (private_idx, public_idx) = match direction {
            PulseDirection::PublicToTun => (tun_idx, pub_idx),
            PulseDirection::TunToPublic => (pub_idx, tun_idx),
        };

        // Private first — the documented, script-verified order.
        conns[private_idx].enable_sharing(ShareRole::Private)?;
        sleep(Duration::from_millis(800));
        conns[public_idx].enable_sharing(ShareRole::Public)?;

        info!("ICS sharing held for {}s", hold_sec);
        sleep(Duration::from_secs(hold_sec.max(1)));
        Ok(())
    }

    fn disable_all(&self, conns: &[IcsConnection], report: &mut PulseReport) {
        for c in conns {
            if c.sharing_enabled {
                info!("disabling ICS on '{}'", c.name);
                if let Err(e) = c.disable_sharing() {
                    warn!("DisableSharing failed on '{}': {}", c.name, e);
                    report.disable_failures.push(c.name.clone());
                }
                sleep(Duration::from_millis(500));
            }
        }
    }
}

impl Drop for IcsPulser {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

/// Iterate a COM collection via `_NewEnum` (DISPID -4).
fn enum_collection(coll: &IDispatch) -> Result<Vec<IDispatch>> {
    let mut enum_var = invoke_dispid(coll, DISPID_NEWENUM, DISPATCH_FLAGS(1 | 2), &[])
        .context("_NewEnum failed")?;
    let unknown = var_take_unknown(&mut enum_var).context("_NewEnum did not return an IUnknown")?;
    let enumerator: IEnumVARIANT = unknown
        .cast()
        .context("_NewEnum result is not IEnumVARIANT")?;

    let mut out = Vec::new();
    let mut item = VARIANT::default();
    let mut fetched: u32 = 0;
    loop {
        let hr = unsafe { enumerator.Next(std::slice::from_mut(&mut item), &mut fetched) };
        if hr.is_err() || fetched == 0 {
            break;
        }
        if let Some(d) = var_take_dispatch(&mut item) {
            out.push(d);
        } else {
            unsafe { VariantClear(&mut item) }.ok();
        }
    }
    Ok(out)
}

unsafe fn clsid_from_progid() -> Result<GUID> {
    use windows::Win32::System::Com::CLSIDFromProgID;
    let wide: Vec<u16> = PROG_ID.encode_utf16().chain(std::iter::once(0)).collect();
    CLSIDFromProgID(PCWSTR(wide.as_ptr())).context("CLSIDFromProgID failed")
}

/// Human-readable summary of one pulse run (also shown in the UI).
#[derive(Debug, Default)]
pub struct PulseReport {
    pub tun_adapter: String,
    pub public_adapter: String,
    pub preexisting_sharing: Vec<String>,
    pub disable_failures: Vec<String>,
    pub restore_failures: Vec<String>,
}

//! A small WMI client over COM: connect to a namespace, call a method on the single instance of a class,
//! wait for events. Only what the Acer classes in `root\WMI` need.

use std::collections::HashMap;
use std::mem::ManuallyDrop;

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    CoInitializeSecurity, CoSetProxyBlanket, EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL,
    RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE, SAFEARRAY,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayCreateVector, SafeArrayGetUBound, SafeArrayUnaccessData,
};
use windows::Win32::System::Variant::{
    VARENUM, VARIANT, VT_ARRAY, VT_BSTR, VT_I1, VT_I2, VT_I4, VT_I8, VT_UI1, VT_UI2, VT_UI4,
    VT_UI8, VariantClear,
};
use windows::Win32::System::Wmi::{
    IEnumWbemClassObject, IWbemClassObject, IWbemLocator, IWbemServices, WBEM_FLAG_FORWARD_ONLY,
    WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_FLAG_RETURN_WBEM_COMPLETE, WBEM_GENERIC_FLAG_TYPE,
    WbemLocator,
};
use windows::core::{BSTR, HSTRING, PCWSTR};

pub type Result<T> = std::result::Result<T, String>;

fn err(what: &str, e: windows::core::Error) -> String {
    format!("{what}: {e}")
}

/// Method argument as WMI wants it. CIM uint64 travels as a decimal string (VT_BSTR).
pub enum Arg {
    U64(u64),
    U32(u32),
    U8(u8),
    Bytes(Vec<u8>),
}

#[derive(Debug, Clone)]
pub enum Value {
    Int(u64),
    Bytes(Vec<u8>),
    None,
}

impl Value {
    pub fn int(&self) -> Option<u64> {
        match self {
            Value::Int(v) => Some(*v),
            Value::Bytes(b) => b.first().map(|v| *v as u64),
            Value::None => None,
        }
    }
}

/// Initialises COM for the calling thread (multithreaded apartment) once per thread.
pub fn init_com() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        // Fails harmlessly when another part of the process already set security.
        let _ = CoInitializeSecurity(
            None,
            -1,
            None,
            None,
            RPC_C_AUTHN_LEVEL_DEFAULT,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
            None,
        );
    }
}

pub struct Wmi {
    svc: IWbemServices,
    paths: HashMap<String, BSTR>,
    classes: HashMap<String, IWbemClassObject>,
}

impl Wmi {
    pub fn connect(namespace: &str) -> Result<Self> {
        unsafe {
            let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| err("WbemLocator", e))?;
            let svc = locator
                .ConnectServer(
                    &BSTR::from(namespace),
                    &BSTR::new(),
                    &BSTR::new(),
                    &BSTR::new(),
                    0,
                    &BSTR::new(),
                    None,
                )
                .map_err(|e| err("ConnectServer", e))?;
            CoSetProxyBlanket(
                &svc,
                10, // RPC_C_AUTHN_WINNT
                0,  // RPC_C_AUTHZ_NONE
                PCWSTR::null(),
                RPC_C_AUTHN_LEVEL_CALL,
                RPC_C_IMP_LEVEL_IMPERSONATE,
                None,
                EOAC_NONE,
            )
            .map_err(|e| err("CoSetProxyBlanket", e))?;
            Ok(Self {
                svc,
                paths: HashMap::new(),
                classes: HashMap::new(),
            })
        }
    }

    /// True when the class has an instance (the hardware interface exists on this machine).
    pub fn has_instance(&mut self, class: &str) -> bool {
        self.instance_path(class).is_ok()
    }

    fn instance_path(&mut self, class: &str) -> Result<BSTR> {
        if let Some(p) = self.paths.get(class) {
            return Ok(p.clone());
        }
        unsafe {
            let en = self
                .svc
                .ExecQuery(
                    &BSTR::from("WQL"),
                    &BSTR::from(format!("SELECT * FROM {class}")),
                    WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                    None,
                )
                .map_err(|e| err("ExecQuery", e))?;
            let obj = next_object(&en, 5000).ok_or_else(|| format!("no instance of {class}"))?;
            let path = match get_prop(&obj, "__PATH") {
                Some(RawVal::Str(s)) => BSTR::from(s),
                _ => return Err(format!("{class} has no __PATH")),
            };
            self.paths.insert(class.to_string(), path.clone());
            Ok(path)
        }
    }

    fn class(&mut self, class: &str) -> Result<IWbemClassObject> {
        if let Some(c) = self.classes.get(class) {
            return Ok(c.clone());
        }
        unsafe {
            let mut obj: Option<IWbemClassObject> = None;
            self.svc
                .GetObject(
                    &BSTR::from(class),
                    WBEM_FLAG_RETURN_WBEM_COMPLETE,
                    None,
                    Some(&mut obj),
                    None,
                )
                .map_err(|e| err("GetObject", e))?;
            let obj = obj.ok_or("GetObject returned nothing")?;
            self.classes.insert(class.to_string(), obj.clone());
            Ok(obj)
        }
    }

    /// Calls `class.method(args)` on the class's instance and returns the named outputs.
    pub fn call(
        &mut self,
        class: &str,
        method: &str,
        args: &[(&str, Arg)],
        outputs: &[&str],
    ) -> Result<Vec<Value>> {
        let path = self.instance_path(class)?;
        let class_obj = self.class(class)?;
        unsafe {
            let mut in_sig: Option<IWbemClassObject> = None;
            class_obj
                .GetMethod(&HSTRING::from(method), 0, &mut in_sig, std::ptr::null_mut())
                .map_err(|e| err("GetMethod", e))?;
            let in_params = match in_sig {
                Some(sig) => Some(sig.SpawnInstance(0).map_err(|e| err("SpawnInstance", e))?),
                None => None,
            };
            if let Some(inp) = &in_params {
                for (name, arg) in args {
                    let mut v = to_variant(arg);
                    let r = inp.Put(&HSTRING::from(*name), 0, &v, 0);
                    let _ = VariantClear(&mut v);
                    r.map_err(|e| err(&format!("Put {name}"), e))?;
                }
            }
            let mut out: Option<IWbemClassObject> = None;
            self.svc
                .ExecMethod(
                    &path,
                    &BSTR::from(method),
                    WBEM_GENERIC_FLAG_TYPE(0),
                    None,
                    in_params.as_ref(),
                    Some(&mut out),
                    None,
                )
                .map_err(|e| {
                    // A stale connection after sleep shows up here; the caller reconnects.
                    self.paths.clear();
                    err(&format!("ExecMethod {method}"), e)
                })?;
            let out = match out {
                Some(o) => o,
                None => return Ok(outputs.iter().map(|_| Value::None).collect()),
            };
            Ok(outputs
                .iter()
                .map(|name| match get_prop(&out, name) {
                    Some(RawVal::Int(v)) => Value::Int(v),
                    Some(RawVal::Str(s)) => s
                        .trim()
                        .parse::<u64>()
                        .map(Value::Int)
                        .unwrap_or(Value::None),
                    Some(RawVal::Bytes(b)) => Value::Bytes(b),
                    None => Value::None,
                })
                .collect())
        }
    }

    /// A numeric property of the class's (first) instance.
    pub fn get_u64(&mut self, class: &str, prop: &str) -> Result<u64> {
        unsafe {
            let en = self
                .svc
                .ExecQuery(
                    &BSTR::from("WQL"),
                    &BSTR::from(format!("SELECT {prop} FROM {class}")),
                    WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                    None,
                )
                .map_err(|e| err("ExecQuery", e))?;
            let obj = next_object(&en, 5000).ok_or_else(|| format!("no instance of {class}"))?;
            match get_prop(&obj, prop) {
                Some(RawVal::Int(v)) => Ok(v),
                Some(RawVal::Str(s)) => s.trim().parse().map_err(|_| format!("{prop}: {s}")),
                _ => Err(format!("{class}.{prop} unreadable")),
            }
        }
    }

    /// Starts an event query; the returned enumerator is read with [`next_event`].
    pub fn subscribe(&self, query: &str) -> Result<IEnumWbemClassObject> {
        unsafe {
            self.svc
                .ExecNotificationQuery(
                    &BSTR::from("WQL"),
                    &BSTR::from(query),
                    WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                    None,
                )
                .map_err(|e| err("ExecNotificationQuery", e))
        }
    }
}

/// Waits up to `timeout_ms` for the next event and returns its named byte-array property.
/// Ok(None) on timeout; Err when the subscription is dead (reconnect).
pub fn next_event(
    en: &IEnumWbemClassObject,
    timeout_ms: i32,
    prop: &str,
) -> Result<Option<Vec<u8>>> {
    let Some(obj) = try_next(en, timeout_ms)? else {
        return Ok(None);
    };
    unsafe {
        Ok(Some(match get_prop(&obj, prop) {
            Some(RawVal::Bytes(b)) => b,
            Some(RawVal::Int(v)) => v.to_le_bytes().to_vec(),
            _ => Vec::new(),
        }))
    }
}

fn try_next(en: &IEnumWbemClassObject, timeout_ms: i32) -> Result<Option<IWbemClassObject>> {
    unsafe {
        let mut objs: [Option<IWbemClassObject>; 1] = [None];
        let mut returned = 0u32;
        let hr = en.Next(timeout_ms, &mut objs, &mut returned);
        if hr.is_err() {
            return Err(format!("event enumerator: {hr:?}"));
        }
        Ok(if returned == 0 { None } else { objs[0].take() })
    }
}

fn next_object(en: &IEnumWbemClassObject, timeout_ms: i32) -> Option<IWbemClassObject> {
    try_next(en, timeout_ms).ok().flatten()
}

enum RawVal {
    Int(u64),
    Str(String),
    Bytes(Vec<u8>),
}

unsafe fn get_prop(obj: &IWbemClassObject, name: &str) -> Option<RawVal> {
    unsafe {
        let mut v = VARIANT::default();
        obj.Get(&HSTRING::from(name), 0, &mut v, None, None).ok()?;
        let r = from_variant(&v);
        let _ = VariantClear(&mut v);
        r
    }
}

unsafe fn from_variant(v: &VARIANT) -> Option<RawVal> {
    unsafe {
        let inner = &v.Anonymous.Anonymous;
        let vt = inner.vt;
        let u = &inner.Anonymous;
        if vt.0 & VT_ARRAY.0 != 0 {
            return Some(RawVal::Bytes(read_byte_array(u.parray)));
        }
        Some(match vt {
            VT_BSTR => RawVal::Str(u.bstrVal.to_string()),
            VT_I1 | VT_UI1 => RawVal::Int(u.bVal as u64),
            VT_I2 | VT_UI2 => RawVal::Int(u.uiVal as u64),
            VT_I4 | VT_UI4 => RawVal::Int(u.ulVal as u64),
            VT_I8 | VT_UI8 => RawVal::Int(u.ullVal),
            _ => return None,
        })
    }
}

unsafe fn read_byte_array(sa: *mut SAFEARRAY) -> Vec<u8> {
    unsafe {
        if sa.is_null() {
            return Vec::new();
        }
        let n = match SafeArrayGetUBound(sa, 1) {
            Ok(ub) if ub >= 0 => (ub + 1) as usize,
            _ => return Vec::new(),
        };
        let mut data = std::ptr::null_mut();
        if SafeArrayAccessData(sa, &mut data).is_err() {
            return Vec::new();
        }
        let out = std::slice::from_raw_parts(data as *const u8, n).to_vec();
        let _ = SafeArrayUnaccessData(sa);
        out
    }
}

fn to_variant(arg: &Arg) -> VARIANT {
    let mut v = VARIANT::default();
    unsafe {
        let inner = &mut *v.Anonymous.Anonymous;
        match arg {
            Arg::U64(x) => {
                inner.vt = VT_BSTR;
                inner.Anonymous.bstrVal = ManuallyDrop::new(BSTR::from(x.to_string()));
            }
            Arg::U32(x) => {
                inner.vt = VT_I4;
                inner.Anonymous.lVal = *x as i32;
            }
            Arg::U8(x) => {
                inner.vt = VT_UI1;
                inner.Anonymous.bVal = *x;
            }
            Arg::Bytes(b) => {
                let sa = SafeArrayCreateVector(VT_UI1, 0, b.len() as u32);
                if !sa.is_null() {
                    let mut data = std::ptr::null_mut();
                    if SafeArrayAccessData(sa, &mut data).is_ok() {
                        std::ptr::copy_nonoverlapping(b.as_ptr(), data as *mut u8, b.len());
                        let _ = SafeArrayUnaccessData(sa);
                    }
                    inner.vt = VARENUM(VT_ARRAY.0 | VT_UI1.0);
                    inner.Anonymous.parray = sa;
                }
            }
        }
    }
    v
}

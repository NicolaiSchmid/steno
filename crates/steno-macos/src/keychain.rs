//! The file-based keychain calls `security-framework` has no safe wrapper
//! for. Swift: `IdentityKeychain` in
//! `Sources/StenoHandover/Identity/IdentityKeychain.swift`, which stores the
//! handover identity these calls read back.
//!
//! [`export_pkcs12`] is the product's one call: the Swift identity leaves
//! the keychain once, as PKCS#12 under a passphrase, which macOS lets
//! through only after the user typed the login password and allowed the
//! export. The [`fixture`] calls (feature `testing`) store an identity
//! the way `IdentityKeychain.store` does, so the keychain test exports
//! what the Swift app would have stored.
#![allow(unsafe_code)]

use std::ptr;

use core_foundation::base::TCFType;
use core_foundation::data::CFData;
use core_foundation::string::CFString;
use security_framework::identity::SecIdentity;
use security_framework_sys::base::errSecSuccess;
use security_framework_sys::import_export::{
    SEC_KEY_IMPORT_EXPORT_PARAMS_VERSION, SecItemExport, SecItemImportExportKeyParameters,
    kSecFormatPKCS12,
};

/// A Security framework call that did not return `errSecSuccess`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{call} failed with OSStatus {status}")]
pub struct KeychainError {
    /// The function that failed, for the log.
    pub call: &'static str,
    /// Its `OSStatus`.
    pub status: i32,
}

impl KeychainError {
    /// `userCanceledErr`: the user chose Deny in the prompt.
    pub const USER_CANCELED: i32 = -128;
    /// `errSecAuthFailed`: the password in the prompt was wrong, or the
    /// prompt was dismissed.
    pub const AUTH_FAILED: i32 = -25293;
    /// `errSecInteractionNotAllowed`: a prompt was needed while user
    /// interaction is disabled (a locked screen, a test keychain).
    pub const INTERACTION_NOT_ALLOWED: i32 = -25308;

    /// Whether the user, or the lack of one, refused the call, as opposed
    /// to the item being unusable.
    #[must_use]
    pub fn is_denied(&self) -> bool {
        matches!(
            self.status,
            Self::USER_CANCELED | Self::AUTH_FAILED | Self::INTERACTION_NOT_ALLOWED
        )
    }

    fn check(call: &'static str, status: i32) -> Result<(), Self> {
        if status == errSecSuccess {
            Ok(())
        } else {
            Err(KeychainError { call, status })
        }
    }
}

/// `identity` (its certificate and private key) as a PKCS#12 file
/// encrypted under `passphrase`. macOS asks the user for the login password
/// before the key leaves the keychain, unless this process is already on
/// the key's access list; the call blocks until the prompt is answered.
/// The file uses the encryption the Security framework writes, 3DES for
/// the key bag and 40-bit RC2 for the certificate bag. Swift: none (the
/// Swift app never exported its identity).
pub fn export_pkcs12(identity: &SecIdentity, passphrase: &str) -> Result<Vec<u8>, KeychainError> {
    let passphrase = CFString::new(passphrase);
    let parameters = SecItemImportExportKeyParameters {
        version: SEC_KEY_IMPORT_EXPORT_PARAMS_VERSION,
        flags: 0,
        passphrase: passphrase.as_CFTypeRef(),
        alertTitle: ptr::null(),
        alertPrompt: ptr::null(),
        accessRef: ptr::null_mut(),
        keyUsage: ptr::null(),
        keyAttributes: ptr::null(),
    };
    let mut exported = ptr::null();
    // SAFETY: `identity` is a live `SecIdentityRef` (the wrapper holds a
    // reference for the borrow), a valid `secItemOrArray`. `parameters` is
    // a fully initialised version-0 struct whose only non-null field,
    // `passphrase`, is a `CFStringRef` kept alive by `passphrase` until the
    // end of this function, after the call. `exported` is a valid
    // out-pointer the call writes a `CFDataRef` (or nothing) into.
    let status = unsafe {
        SecItemExport(
            identity.as_CFTypeRef(),
            kSecFormatPKCS12,
            0,
            &raw const parameters,
            &raw mut exported,
        )
    };
    KeychainError::check("SecItemExport", status)?;
    if exported.is_null() {
        return Err(KeychainError {
            call: "SecItemExport",
            status: security_framework_sys::base::errSecItemNotFound,
        });
    }
    // SAFETY: on success `exportedData` follows the Create rule: the call
    // returned a +1 `CFDataRef`, checked non-null above, which the wrapper
    // now owns and releases once.
    let data = unsafe { CFData::wrap_under_create_rule(exported) };
    Ok(data.bytes().to_vec())
}

/// The keychain test's fixture calls: an identity stored the way
/// `IdentityKeychain.store` stores it, into a keychain the test owns, and
/// that keychain's deletion.
#[cfg(feature = "testing")]
pub mod fixture {
    use std::ptr;

    use core_foundation::array::CFArray;
    use core_foundation::base::{CFType, TCFType};
    use core_foundation::data::CFData;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::{CFString, CFStringRef};
    use security_framework::certificate::SecCertificate;
    use security_framework::key::SecKey;
    use security_framework::os::macos::keychain::SecKeychain;
    use security_framework_sys::base::{SecKeychainRef, errSecDuplicateItem};
    use security_framework_sys::import_export::{
        SEC_KEY_IMPORT_EXPORT_PARAMS_VERSION, SecExternalFormat, SecExternalItemType,
        SecItemImport, SecItemImportExportKeyParameters, kSecFormatOpenSSL,
    };
    use security_framework_sys::item::{
        kSecAttrLabel, kSecClass, kSecClassCertificate, kSecClassKey, kSecValueRef,
    };
    use security_framework_sys::keychain_item::SecItemUpdate;

    use super::KeychainError;

    /// `kSecItemTypePrivateKey` in `SecImportExport.h`; the sys crate
    /// declares the type but not the cases.
    const ITEM_TYPE_PRIVATE_KEY: SecExternalItemType = 1;

    #[link(name = "Security", kind = "framework")]
    unsafe extern "C" {
        static kSecAttrApplicationTag: CFStringRef;
        fn SecKeychainDelete(keychain: SecKeychainRef) -> i32;
    }

    /// A Security framework constant as a `CFString`.
    ///
    /// # Safety
    ///
    /// `key` must be one of the framework's `CFStringRef` constants: they
    /// are initialised before `main` and live for the whole process.
    unsafe fn constant(key: CFStringRef) -> CFString {
        // SAFETY: the caller's contract; the get rule retains the constant
        // and the release on drop balances that retain.
        unsafe { CFString::wrap_under_get_rule(key) }
    }

    /// Imports a P-256 private key in SEC1 (RFC 5915) DER into `keychain`
    /// through the OpenSSL importer, as `IdentityKeychain.store` does
    /// (#88: nothing else puts a software key into a file keychain). The
    /// key, or `None` when the keychain already holds it.
    pub fn import_sec1_private_key(
        der: &[u8],
        keychain: &SecKeychain,
    ) -> Result<Option<SecKey>, KeychainError> {
        let data = CFData::from_buffer(der);
        let mut format: SecExternalFormat = kSecFormatOpenSSL;
        let mut item_type = ITEM_TYPE_PRIVATE_KEY;
        let parameters = SecItemImportExportKeyParameters {
            version: SEC_KEY_IMPORT_EXPORT_PARAMS_VERSION,
            flags: 0,
            passphrase: ptr::null(),
            alertTitle: ptr::null(),
            alertPrompt: ptr::null(),
            accessRef: ptr::null_mut(),
            keyUsage: ptr::null(),
            keyAttributes: ptr::null(),
        };
        let mut imported = ptr::null();
        // SAFETY: `data` and `keychain` are live references owned by this
        // frame for the whole call; `format` and `item_type` are valid
        // in-out pointers to initialised values; `parameters` is a fully
        // initialised version-0 struct with no references; no file name
        // hint (null is allowed); `imported` is a valid out-pointer.
        let status = unsafe {
            SecItemImport(
                data.as_concrete_TypeRef(),
                ptr::null(),
                &raw mut format,
                &raw mut item_type,
                0,
                &raw const parameters,
                keychain.as_concrete_TypeRef(),
                &raw mut imported,
            )
        };
        if status == errSecDuplicateItem {
            return Ok(None);
        }
        KeychainError::check("SecItemImport", status)?;
        if imported.is_null() {
            return Ok(None);
        }
        // SAFETY: on success `outItems` follows the Create rule: a +1
        // `CFArrayRef`, non-null as checked, which the wrapper owns.
        let items: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(imported) };
        Ok(items
            .iter()
            .find(|item| item.type_of() == SecKey::type_id())
            .map(|item| {
                // SAFETY: the item is a `SecKeyRef` (its type id was just
                // checked) that the array keeps alive; the get rule
                // retains it for the returned wrapper.
                unsafe { SecKey::wrap_under_get_rule(item.as_CFTypeRef() as *mut _) }
            }))
    }

    /// Labels a stored key and tags it with the label's bytes, so Keychain
    /// Access and a label query find it, as `IdentityKeychain.store`'s
    /// `SecItemUpdate(key)` does.
    pub fn label_key(key: &SecKey, label: &str) -> Result<(), KeychainError> {
        // SAFETY: the four keys are Security framework constants.
        let (class, class_key, value_ref, label_key, tag_key) = unsafe {
            (
                constant(kSecClass),
                constant(kSecClassKey),
                constant(kSecValueRef),
                constant(kSecAttrLabel),
                constant(kSecAttrApplicationTag),
            )
        };
        let query = CFDictionary::from_CFType_pairs(&[
            (class.as_CFType(), class_key.as_CFType()),
            (value_ref.as_CFType(), key.as_CFType()),
        ]);
        let update = CFDictionary::from_CFType_pairs(&[
            (label_key.as_CFType(), CFString::new(label).as_CFType()),
            (
                tag_key.as_CFType(),
                CFData::from_buffer(label.as_bytes()).as_CFType(),
            ),
        ]);
        update_item(&query, &update, "SecItemUpdate(key)")
    }

    /// Labels a stored certificate, as `IdentityKeychain.store`'s
    /// `SecItemUpdate(certificate)` does: the file keychain labels a
    /// certificate with its common name on add and ignores a label in the
    /// add dictionary.
    pub fn label_certificate(
        certificate: &SecCertificate,
        label: &str,
    ) -> Result<(), KeychainError> {
        // SAFETY: the four keys are Security framework constants.
        let (class, class_certificate, value_ref, label_key) = unsafe {
            (
                constant(kSecClass),
                constant(kSecClassCertificate),
                constant(kSecValueRef),
                constant(kSecAttrLabel),
            )
        };
        let query = CFDictionary::from_CFType_pairs(&[
            (class.as_CFType(), class_certificate.as_CFType()),
            (value_ref.as_CFType(), certificate.as_CFType()),
        ]);
        let update = CFDictionary::from_CFType_pairs(&[(
            label_key.as_CFType(),
            CFString::new(label).as_CFType(),
        )]);
        update_item(&query, &update, "SecItemUpdate(certificate)")
    }

    fn update_item(
        query: &CFDictionary<CFType, CFType>,
        update: &CFDictionary<CFType, CFType>,
        call: &'static str,
    ) -> Result<(), KeychainError> {
        // SAFETY: both dictionaries are live `CFDictionaryRef`s owned by
        // the caller for the whole call; `SecItemUpdate` only reads them.
        let status =
            unsafe { SecItemUpdate(query.as_concrete_TypeRef(), update.as_concrete_TypeRef()) };
        KeychainError::check(call, status)
    }

    /// Deletes `keychain`'s file and removes it from the search list, so a
    /// test leaves nothing behind.
    pub fn delete_keychain(keychain: &SecKeychain) -> Result<(), KeychainError> {
        // SAFETY: `keychain` is a live `SecKeychainRef` the borrow keeps
        // alive for the whole call; `SecKeychainDelete` does not release it.
        let status = unsafe { SecKeychainDelete(keychain.as_concrete_TypeRef()) };
        KeychainError::check("SecKeychainDelete", status)
    }
}

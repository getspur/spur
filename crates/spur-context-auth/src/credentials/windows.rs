//! Windows restricted-file ACL enforcement.

use std::path::Path;

use nt_token::OwnedToken;
use windows::Win32::Security::TOKEN_QUERY;
use windows_permissions::{
    constants::{AccessRights, AceFlags, AceType, SeObjectType, SecurityInformation},
    wrappers::{GetNamedSecurityInfo, SetNamedSecurityInfo},
    LocalBox, SecurityDescriptor, Sid,
};

use super::{windows_acl_is_owner_only, CredentialError};

pub(super) fn restrict_new_file(path: &Path) -> Result<(), CredentialError> {
    let current_user = current_user_sid()?;
    let descriptor: LocalBox<SecurityDescriptor> = format!("D:P(A;;FA;;;{current_user})")
        .parse()
        .map_err(|_error| CredentialError::Backend)?;
    let dacl = descriptor.dacl().ok_or(CredentialError::Backend)?;
    SetNamedSecurityInfo(
        &path.as_os_str(),
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Owner | SecurityInformation::Dacl | SecurityInformation::ProtectedDacl,
        Some(current_user.as_ref()),
        None,
        Some(dacl),
        None,
    )
    .map_err(|_error| CredentialError::Backend)?;
    enforce_restricted(path)
}

pub(super) fn enforce_restricted(path: &Path) -> Result<(), CredentialError> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_error| CredentialError::Backend)?;
    if !metadata.file_type().is_file() {
        return Err(CredentialError::InsecureFilePermissions);
    }

    let current_user = current_user_sid()?;
    let descriptor = GetNamedSecurityInfo(
        &path.as_os_str(),
        SeObjectType::SE_FILE_OBJECT,
        SecurityInformation::Owner | SecurityInformation::Dacl,
    )
    .map_err(|_error| CredentialError::Backend)?;
    let owner_is_current = descriptor.owner() == Some(current_user.as_ref());
    let dacl_is_protected = descriptor
        .as_sddl()
        .ok()
        .and_then(|sddl| sddl.into_string().ok())
        .and_then(|sddl| sddl.split_once("D:").map(|(_, dacl)| dacl.starts_with('P')))
        .unwrap_or(false);
    let dacl = descriptor
        .dacl()
        .ok_or(CredentialError::InsecureFilePermissions)?;
    let ace_matches = (0..dacl.len())
        .map(|index| {
            dacl.get_ace(index).is_some_and(|ace| {
                ace.ace_type() == AceType::ACCESS_ALLOWED_ACE_TYPE
                    && !ace.flags().contains(AceFlags::Inherited)
                    && ace
                        .mask()
                        .contains(AccessRights::FileGenericRead | AccessRights::FileGenericWrite)
                    && ace.sid() == Some(current_user.as_ref())
            })
        })
        .collect::<Vec<_>>();
    if !windows_acl_is_owner_only(owner_is_current, dacl_is_protected, &ace_matches) {
        return Err(CredentialError::InsecureFilePermissions);
    }
    Ok(())
}

fn current_user_sid() -> Result<LocalBox<Sid>, CredentialError> {
    let token =
        OwnedToken::from_current_process(TOKEN_QUERY).map_err(|_error| CredentialError::Backend)?;
    let sid = token
        .user()
        .map_err(|_error| CredentialError::Backend)?
        .to_string()
        .map_err(|_error| CredentialError::Backend)?;
    sid.parse().map_err(|_error| CredentialError::Backend)
}

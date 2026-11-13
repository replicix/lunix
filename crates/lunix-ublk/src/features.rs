//! The driver's feature mask, the flag-set rules found in phase 0 and the
//! choice of flags for `ADD_DEV` (ADR-17).

use std::fmt;

use crate::error::{Result, UblkError};

/// `UBLK_F_*` bits of `ublksrv_ctrl_dev_info.flags` and of `GET_FEATURES`.
pub mod flag {
    /// Zero copy (never used: the server could not read the data).
    pub const SUPPORT_ZERO_COPY: u64 = 1 << 0;
    /// Set by the driver on every device.
    pub const URING_CMD_COMP_IN_TASK: u64 = 1 << 1;
    /// Extra round trip per write (never used).
    pub const NEED_GET_DATA: u64 = 1 << 2;
    /// Mandatory.
    pub const USER_RECOVERY: u64 = 1 << 3;
    /// Mandatory.
    pub const USER_RECOVERY_REISSUE: u64 = 1 << 4;
    /// Not used.
    pub const UNPRIVILEGED_DEV: u64 = 1 << 5;
    /// Mandatory.
    pub const CMD_IOCTL_ENCODE: u64 = 1 << 6;
    /// Opt-in per volume.
    pub const USER_COPY: u64 = 1 << 7;
    /// Never used.
    pub const ZONED: u64 = 1 << 8;
    /// A give-up policy: such a device cannot be recovered.
    pub const USER_RECOVERY_FAIL_IO: u64 = 1 << 9;
    /// Mandatory.
    pub const UPDATE_SIZE: u64 = 1 << 10;
    /// Never used.
    pub const AUTO_BUF_REG: u64 = 1 << 11;
    /// Mandatory.
    pub const QUIESCE: u64 = 1 << 12;
    /// Set by the driver on every device.
    pub const PER_IO_DAEMON: u64 = 1 << 13;
    /// Set by the driver on every device.
    pub const BUF_REG_OFF_DAEMON: u64 = 1 << 14;
    /// Opt-in; never combined with `QUIESCE` (W6).
    pub const BATCH_IO: u64 = 1 << 15;
    /// Not used; needs `USER_COPY`.
    pub const INTEGRITY: u64 = 1 << 16;
    /// Enables `TRY_STOP_DEV`.
    pub const SAFE_STOP_DEV: u64 = 1 << 17;
    /// No partition scan at start.
    pub const NO_AUTO_PART_SCAN: u64 = 1 << 18;
}

/// The first kernel with every mandatory flag.
pub const KERNEL_FLOOR: &str = "6.16";

/// The kernel the layer is tested on.
pub const KERNEL_TESTED: &str = "7.0";

/// Flags the layer cannot work without.
pub const MANDATORY: [(&str, u64); 5] = [
    ("CMD_IOCTL_ENCODE", flag::CMD_IOCTL_ENCODE),
    ("USER_RECOVERY", flag::USER_RECOVERY),
    ("USER_RECOVERY_REISSUE", flag::USER_RECOVERY_REISSUE),
    ("QUIESCE", flag::QUIESCE),
    ("UPDATE_SIZE", flag::UPDATE_SIZE),
];

/// Flags that are never set on a Lunix device.
pub const NEVER: u64 = flag::SUPPORT_ZERO_COPY
    | flag::NEED_GET_DATA
    | flag::AUTO_BUF_REG
    | flag::ZONED
    | flag::UNPRIVILEGED_DEV;

/// One row of the feature table (ADR-17, phase 0 `RESULTS.md`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FlagRow {
    /// Name without the `UBLK_F_` prefix.
    pub name: &'static str,
    /// Bit number.
    pub bit: u32,
    /// How Lunix treats it.
    pub class: &'static str,
    /// First kernel (from the fact-check note, not re-verified).
    pub since: &'static str,
    /// Remark.
    pub note: &'static str,
}

/// The 19 flags of the 7.0 kernel.
pub const FLAG_TABLE: [FlagRow; 19] = [
    FlagRow {
        name: "SUPPORT_ZERO_COPY",
        bit: 0,
        class: "never",
        since: "6.15 functional (6.0 stub)",
        note: "Server cannot read the data (ADR-17: never)",
    },
    FlagRow {
        name: "URING_CMD_COMP_IN_TASK",
        bit: 1,
        class: "ignored",
        since: "6.0",
        note: "Set by the driver itself on every device",
    },
    FlagRow {
        name: "NEED_GET_DATA",
        bit: 2,
        class: "never",
        since: "6.0",
        note: "Extra round trip per write",
    },
    FlagRow {
        name: "USER_RECOVERY",
        bit: 3,
        class: "mandatory",
        since: "6.1",
        note: "",
    },
    FlagRow {
        name: "USER_RECOVERY_REISSUE",
        bit: 4,
        class: "mandatory",
        since: "6.1",
        note: "",
    },
    FlagRow {
        name: "UNPRIVILEGED_DEV",
        bit: 5,
        class: "not used",
        since: "6.3",
        note: "Forbids USER_COPY/ZC",
    },
    FlagRow {
        name: "CMD_IOCTL_ENCODE",
        bit: 6,
        class: "mandatory",
        since: "6.4",
        note: "Legacy opcodes are compiled out on this kernel",
    },
    FlagRow {
        name: "USER_COPY",
        bit: 7,
        class: "opt-in per volume",
        since: "6.5",
        note: "ADR-17: copy path is the default",
    },
    FlagRow {
        name: "ZONED",
        bit: 8,
        class: "never",
        since: "6.6",
        note: "",
    },
    FlagRow {
        name: "USER_RECOVERY_FAIL_IO",
        bit: 9,
        class: "optional policy",
        since: "6.13",
        note: "fail instead of queue while the server is away",
    },
    FlagRow {
        name: "UPDATE_SIZE",
        bit: 10,
        class: "mandatory",
        since: "6.16",
        note: "",
    },
    FlagRow {
        name: "AUTO_BUF_REG",
        bit: 11,
        class: "never",
        since: "6.16",
        note: "",
    },
    FlagRow {
        name: "QUIESCE",
        bit: 12,
        class: "mandatory",
        since: "6.16",
        note: "needs USER_RECOVERY",
    },
    FlagRow {
        name: "PER_IO_DAEMON",
        bit: 13,
        class: "not used",
        since: "6.16",
        note: "Set by the driver itself on every device",
    },
    FlagRow {
        name: "BUF_REG_OFF_DAEMON",
        bit: 14,
        class: "never",
        since: "6.17",
        note: "Set by the driver itself on every device",
    },
    FlagRow {
        name: "BATCH_IO",
        bit: 15,
        class: "opt-in, never with QUIESCE",
        since: "7.0",
        note: "ADR-17: no gain, broke a QUIESCE handover",
    },
    FlagRow {
        name: "INTEGRITY",
        bit: 16,
        class: "not used",
        since: "7.0",
        note: "Requires USER_COPY",
    },
    FlagRow {
        name: "SAFE_STOP_DEV",
        bit: 17,
        class: "used when present",
        since: "7.0",
        note: "Enables TRY_STOP_DEV; set by the driver itself",
    },
    FlagRow {
        name: "NO_AUTO_PART_SCAN",
        bit: 18,
        class: "used when present",
        since: "7.0",
        note: "",
    },
];

/// The mandatory flags a mask lacks (for the refusal message).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MissingFeatures {
    /// Names without the `UBLK_F_` prefix.
    pub names: Vec<&'static str>,
    /// The mask the driver reported.
    pub mask: u64,
}

impl fmt::Display for MissingFeatures {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the ublk driver lacks mandatory feature flag(s) {} (GET_FEATURES mask {:#x}); \
             Lunix needs Linux {} or newer (tested: {})",
            self.names.join(", "),
            self.mask,
            KERNEL_FLOOR,
            KERNEL_TESTED
        )
    }
}

/// A `GET_FEATURES` mask.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Features(pub u64);

impl Features {
    /// True when every bit of `flags` is set.
    pub fn has(self, flags: u64) -> bool {
        self.0 & flags == flags
    }

    /// The mandatory flags this mask lacks, by name.
    pub fn missing_mandatory(self) -> Vec<&'static str> {
        MANDATORY
            .iter()
            .filter(|(_, bit)| !self.has(*bit))
            .map(|(name, _)| *name)
            .collect()
    }

    /// Refuses a driver without every mandatory flag.
    ///
    /// # Errors
    /// [`UblkError::MissingFeatures`], naming the kernel floor.
    pub fn require_mandatory(self) -> Result<()> {
        let names = self.missing_mandatory();
        if names.is_empty() {
            Ok(())
        } else {
            Err(UblkError::MissingFeatures(MissingFeatures {
                names,
                mask: self.0,
            }))
        }
    }

    /// The flags to pass to `ADD_DEV`: the mandatory ones, plus
    /// `SAFE_STOP_DEV` and `NO_AUTO_PART_SCAN` when the driver has them, plus
    /// `USER_COPY` when asked for.
    ///
    /// `reissue` is `true` outside the negative control that omits it.
    ///
    /// # Errors
    /// A missing mandatory flag, `USER_COPY` asked for but absent, or a flag
    /// set that breaks a rule of [`check_flag_rules`].
    pub fn add_flags(self, user_copy: bool, reissue: bool) -> Result<u64> {
        self.require_mandatory()?;
        let mut flags =
            flag::CMD_IOCTL_ENCODE | flag::USER_RECOVERY | flag::QUIESCE | flag::UPDATE_SIZE;
        if reissue {
            flags |= flag::USER_RECOVERY_REISSUE;
        }
        for optional in [flag::SAFE_STOP_DEV, flag::NO_AUTO_PART_SCAN] {
            if self.has(optional) {
                flags |= optional;
            }
        }
        if user_copy {
            if !self.has(flag::USER_COPY) {
                return Err(UblkError::Unsupported("the driver has no USER_COPY"));
            }
            flags |= flag::USER_COPY;
        }
        check_flag_rules(flags)?;
        Ok(flags)
    }

    /// The markdown table of `lunix-ublk-testd features` (the format of the
    /// phase 0 table in `bench/ublk-recovery/RESULTS.md`).
    pub fn render_table(self) -> String {
        let mut out = format!("GET_FEATURES mask = {:#x}\n\n", self.0);
        out.push_str(
            "| flag | bit | set on this kernel | ADR-17 class | first kernel (fact-check §E, not re-verified) | note |\n",
        );
        out.push_str("|---|---|---|---|---|---|\n");
        for r in &FLAG_TABLE {
            let set = if self.0 >> r.bit & 1 == 1 {
                "yes"
            } else {
                "no"
            };
            out.push_str(&format!(
                "| UBLK_F_{} | {} | {} | {} | {} | {} |\n",
                r.name, r.bit, set, r.class, r.since, r.note
            ));
        }
        out
    }
}

/// Checks the flag-set rules found by probing `ADD_DEV` in phase 0 and the
/// rules of ADR-17. These are the combinations the kernel rejects or that
/// Lunix forbids.
///
/// # Errors
/// [`UblkError::Config`] naming the rule.
pub fn check_flag_rules(flags: u64) -> Result<()> {
    let has = |f: u64| flags & f == f;
    if has(flag::QUIESCE) && !has(flag::USER_RECOVERY) {
        return Err(UblkError::Config("QUIESCE needs USER_RECOVERY"));
    }
    if has(flag::USER_RECOVERY_REISSUE) && !has(flag::USER_RECOVERY) {
        return Err(UblkError::Config("REISSUE needs USER_RECOVERY"));
    }
    if has(flag::USER_RECOVERY_FAIL_IO) && !has(flag::USER_RECOVERY) {
        return Err(UblkError::Config("FAIL_IO needs USER_RECOVERY"));
    }
    if has(flag::USER_RECOVERY_REISSUE) && has(flag::USER_RECOVERY_FAIL_IO) {
        return Err(UblkError::Config("REISSUE excludes FAIL_IO"));
    }
    if has(flag::INTEGRITY) && !has(flag::USER_COPY) {
        return Err(UblkError::Config("INTEGRITY needs USER_COPY"));
    }
    if has(flag::BATCH_IO) && has(flag::QUIESCE) {
        return Err(UblkError::Config(
            "BATCH_IO is never combined with QUIESCE (ADR-17, W6)",
        ));
    }
    if flags & NEVER != 0 {
        return Err(UblkError::Config(
            "NEED_GET_DATA, zero copy, AUTO_BUF_REG, ZONED and UNPRIVILEGED_DEV are never used",
        ));
    }
    if !has(flag::CMD_IOCTL_ENCODE) {
        return Err(UblkError::Config("CMD_IOCTL_ENCODE is mandatory"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOST_MASK: u64 = 0x7ffff;

    #[test]
    fn host_mask_has_every_mandatory_flag() {
        assert!(Features(HOST_MASK).require_mandatory().is_ok());
    }

    #[test]
    fn a_mocked_mask_without_quiesce_is_refused_with_the_floor() {
        let mask = HOST_MASK & !flag::QUIESCE & !flag::UPDATE_SIZE;
        let err = Features(mask).require_mandatory().unwrap_err();
        let text = err.to_string();
        assert!(text.contains("QUIESCE"), "{text}");
        assert!(text.contains("UPDATE_SIZE"), "{text}");
        assert!(text.contains("Linux 6.16 or newer"), "{text}");
        assert!(matches!(err, UblkError::MissingFeatures(_)));
        assert!(Features(mask).add_flags(false, true).is_err());
    }

    #[test]
    fn an_empty_mask_lists_all_five_mandatory_flags() {
        assert_eq!(Features(0).missing_mandatory().len(), 5);
    }

    #[test]
    fn add_flags_match_the_probed_values() {
        // CMD_IOCTL_ENCODE | USER_RECOVERY | REISSUE | UPDATE_SIZE | QUIESCE
        // plus SAFE_STOP_DEV and NO_AUTO_PART_SCAN on the 7.0 kernel.
        let f = Features(HOST_MASK).add_flags(false, true).unwrap();
        assert_eq!(f, 0x40 | 0x8 | 0x10 | 0x400 | 0x1000 | 0x20000 | 0x40000);
        // On a 6.16 kernel without the two optional flags.
        let old = HOST_MASK & !flag::SAFE_STOP_DEV & !flag::NO_AUTO_PART_SCAN;
        let f = Features(old).add_flags(false, true).unwrap();
        assert_eq!(f, 0x40 | 0x8 | 0x10 | 0x400 | 0x1000);
    }

    #[test]
    fn user_copy_is_opt_in_and_needs_the_driver_flag() {
        let f = Features(HOST_MASK).add_flags(true, true).unwrap();
        assert!(f & flag::USER_COPY != 0);
        let no_copy = HOST_MASK & !flag::USER_COPY;
        assert!(Features(no_copy).add_flags(true, true).is_err());
        let default = Features(HOST_MASK).add_flags(false, true).unwrap();
        assert_eq!(default & flag::USER_COPY, 0);
    }

    #[test]
    fn the_flag_set_rules_found_in_phase_0() {
        use flag::*;
        let base = CMD_IOCTL_ENCODE | USER_RECOVERY;
        // QUIESCE needs USER_RECOVERY.
        assert!(check_flag_rules(CMD_IOCTL_ENCODE | QUIESCE).is_err());
        assert!(check_flag_rules(base | QUIESCE).is_ok());
        // REISSUE and FAIL_IO exclude each other.
        assert!(check_flag_rules(base | USER_RECOVERY_REISSUE | USER_RECOVERY_FAIL_IO).is_err());
        assert!(check_flag_rules(base | USER_RECOVERY_FAIL_IO).is_ok());
        assert!(check_flag_rules(base | USER_RECOVERY_REISSUE).is_ok());
        // INTEGRITY needs USER_COPY.
        assert!(check_flag_rules(base | INTEGRITY).is_err());
        assert!(check_flag_rules(base | INTEGRITY | USER_COPY).is_ok());
        // BATCH_IO never with QUIESCE.
        assert!(check_flag_rules(base | BATCH_IO | QUIESCE).is_err());
        assert!(check_flag_rules(base | BATCH_IO).is_ok());
        // Never-used flags.
        for never in [
            NEED_GET_DATA,
            SUPPORT_ZERO_COPY,
            AUTO_BUF_REG,
            ZONED,
            UNPRIVILEGED_DEV,
        ] {
            assert!(check_flag_rules(base | never).is_err());
        }
        // Legacy opcodes are not available.
        assert!(check_flag_rules(USER_RECOVERY).is_err());
        // Without USER_RECOVERY the recovery flags are meaningless.
        assert!(check_flag_rules(CMD_IOCTL_ENCODE | USER_RECOVERY_REISSUE).is_err());
    }

    #[test]
    fn the_table_has_one_row_per_bit_in_order() {
        for (i, r) in FLAG_TABLE.iter().enumerate() {
            assert_eq!(r.bit as usize, i);
        }
        let text = Features(HOST_MASK).render_table();
        assert!(text.starts_with("GET_FEATURES mask = 0x7ffff\n"));
        assert_eq!(text.matches("| yes |").count(), 19);
        let none = Features(0).render_table();
        assert_eq!(none.matches("| no |").count(), 19);
    }

    #[test]
    fn named_bits_match_the_table() {
        let by_name = |n: &str| {
            FLAG_TABLE
                .iter()
                .find(|r| r.name == n)
                .map(|r| 1u64 << r.bit)
        };
        assert_eq!(by_name("QUIESCE"), Some(flag::QUIESCE));
        assert_eq!(by_name("USER_COPY"), Some(flag::USER_COPY));
        assert_eq!(by_name("SAFE_STOP_DEV"), Some(flag::SAFE_STOP_DEV));
        assert_eq!(by_name("NO_AUTO_PART_SCAN"), Some(flag::NO_AUTO_PART_SCAN));
        assert_eq!(by_name("UPDATE_SIZE"), Some(flag::UPDATE_SIZE));
    }

    #[test]
    fn bits_equal_the_kernel_header_constants() {
        use libublk::sys as k;
        let pairs: [(u64, u64); 19] = [
            (flag::SUPPORT_ZERO_COPY, k::UBLK_F_SUPPORT_ZERO_COPY as u64),
            (
                flag::URING_CMD_COMP_IN_TASK,
                k::UBLK_F_URING_CMD_COMP_IN_TASK as u64,
            ),
            (flag::NEED_GET_DATA, k::UBLK_F_NEED_GET_DATA as u64),
            (flag::USER_RECOVERY, k::UBLK_F_USER_RECOVERY as u64),
            (
                flag::USER_RECOVERY_REISSUE,
                k::UBLK_F_USER_RECOVERY_REISSUE as u64,
            ),
            (flag::UNPRIVILEGED_DEV, k::UBLK_F_UNPRIVILEGED_DEV as u64),
            (flag::CMD_IOCTL_ENCODE, k::UBLK_F_CMD_IOCTL_ENCODE as u64),
            (flag::USER_COPY, k::UBLK_F_USER_COPY as u64),
            (flag::ZONED, k::UBLK_F_ZONED as u64),
            (
                flag::USER_RECOVERY_FAIL_IO,
                k::UBLK_F_USER_RECOVERY_FAIL_IO as u64,
            ),
            (flag::UPDATE_SIZE, k::UBLK_F_UPDATE_SIZE as u64),
            (flag::AUTO_BUF_REG, k::UBLK_F_AUTO_BUF_REG as u64),
            (flag::QUIESCE, k::UBLK_F_QUIESCE as u64),
            (flag::PER_IO_DAEMON, k::UBLK_F_PER_IO_DAEMON as u64),
            (
                flag::BUF_REG_OFF_DAEMON,
                k::UBLK_F_BUF_REG_OFF_DAEMON as u64,
            ),
            (flag::BATCH_IO, k::UBLK_F_BATCH_IO as u64),
            (flag::INTEGRITY, k::UBLK_F_INTEGRITY as u64),
            (flag::SAFE_STOP_DEV, k::UBLK_F_SAFE_STOP_DEV as u64),
            (flag::NO_AUTO_PART_SCAN, k::UBLK_F_NO_AUTO_PART_SCAN as u64),
        ];
        for (ours, theirs) in pairs {
            assert_eq!(ours, theirs);
        }
    }
}

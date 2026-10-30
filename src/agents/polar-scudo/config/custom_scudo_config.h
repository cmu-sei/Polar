/*
   ACE Tool

   Copyright 2026 Carnegie Mellon University.

   NO WARRANTY. THIS CARNEGIE MELLON UNIVERSITY AND SOFTWARE ENGINEERING INSTITUTE MATERIAL IS
   FURNISHED ON AN "AS-IS" BASIS. CARNEGIE MELLON UNIVERSITY MAKES NO WARRANTIES OF ANY KIND, EITHER
   EXPRESSED OR IMPLIED, AS TO ANY MATTER INCLUDING, BUT NOT LIMITED TO, WARRANTY OF FITNESS FOR
   PURPOSE OR MERCHANTABILITY, EXCLUSIVITY, OR RESULTS OBTAINED FROM USE OF THE MATERIAL. CARNEGIE
   MELLON UNIVERSITY DOES NOT MAKE ANY WARRANTY OF ANY KIND WITH RESPECT TO FREEDOM FROM PATENT,
   TRADEMARK, OR COPYRIGHT INFRINGEMENT.

   Licensed under a MIT (SEI)-style license, please see license.txt or contact
   permission@sei.cmu.edu for full terms.

   [DISTRIBUTION STATEMENT A] This material has been approved for public release and unlimited
   distribution.  Please see Copyright notice for non-US Government use and distribution.

   This Software includes and/or makes use of Third-Party Software each subject to its own license.

   DM26-0670
*/

// Every Polar binary's allocator: S4 of the allocator experiment (docs/architecture/broker-baseline.md, "The third
// set"). Upstream's DefaultConfig (allocator_config.h) with four changes: freed blocks up to 4 MiB kept in the
// large-block cache (upstream keeps none above 512 KiB, so every 1 MiB message was a fresh mmap and munmap); the
// quarantine compiled out (it is off by default); freed memory released to the system after 1 s; and the futex
// condition variable for the primary's refills. One thread cache a thread, as upstream: shared ones take a lock on every
// allocation and free, and held small messages to about four fifths of what this carries.
// Included by Scudo's allocator_config.h (SCUDO_USE_CUSTOM_CONFIG); the fields are allocator_config.def's. 64-bit
// targets only (x86_64, aarch64).
#ifndef POLAR_SCUDO_CONFIG_H_
#define POLAR_SCUDO_CONFIG_H_

#include "condition_variable.h"

namespace scudo {

struct PolarConfig {
  static const bool MaySupportMemoryTagging = true;
  static const bool QuarantineDisabled = true;
  template <class A> using TSDRegistryT = TSDRegistryExT<A>; // one cache a thread;

  struct Primary {
    using SizeClassMap = DefaultSizeClassMap;
    static const uptr RegionSizeLog = 32U;
    static const uptr GroupSizeLog = 21U;
    typedef uptr CompactPtrT;
    static const uptr CompactPtrScale = 0;
    static const bool EnableRandomOffset = true;
    static const uptr MapSizeIncrement = 1UL << 18;
    static const s32 MinReleaseToOsIntervalMs = 1000;
    static const s32 MaxReleaseToOsIntervalMs = 1000;
    using ConditionVariableT = ConditionVariableLinux;
  };
  template <typename Config> using PrimaryT = SizeClassAllocator64<Config>;

  struct Secondary {
    struct Cache {
      static const u32 EntriesArraySize = 32U;
      static const u32 QuarantineSize = 0U;
      static const u32 DefaultMaxEntriesCount = 32U;
      static const uptr DefaultMaxEntrySize = 4UL << 20;
      static const s32 MinReleaseToOsIntervalMs = 1000;
      static const s32 MaxReleaseToOsIntervalMs = 1000;
      static const s32 DefaultReleaseToOsIntervalMs = 1000;
    };
    template <typename Config> using CacheT = MapAllocatorCache<Config>;
  };
  template <typename Config> using SecondaryT = MapAllocator<Config>;
};

typedef PolarConfig Config;
typedef PolarConfig DefaultConfig;

} // namespace scudo

#endif // POLAR_SCUDO_CONFIG_H_

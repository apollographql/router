//! Symbol resolution for jemalloc heap profiles
//!
//! Transforms raw heap dumps from jemalloc (containing only hex addresses) into
//! self-contained profiles with embedded function names. This makes profiles portable
//! and analyzable without requiring access to the original router binary.
//!
//! ## Problem
//!
//! Raw jemalloc heap dumps contain memory addresses but no function names:
//! ```text
//! @ 0x55a1b2c3d4e5 0x55a1b2c3d678 0x55a1b2c3d890
//!   t1: 1024: 8192 [0: 0]
//! ```
//!
//! These addresses are meaningless without the binary they came from. Tools like
//! `jeprof` require the original binary to resolve addresses to function names.
//!
//! ## Solution
//!
//! This module uses `addr2line` to resolve all addresses in the heap dump to function
//! names, then appends a symbol table to the `.prof` file. The enhanced profile becomes
//! self-contained and portable.
//!
//! ## Enhanced Profile Format
//!
//! The module appends a symbol section to the original heap dump:
//! ```text
//! [original heap data]
//! --- symbol
//! binary=/path/to/router
//! 0x55a1b2c3d4e5 apollo_router::services::execution::execute
//! 0x55a1b2c3d678 tokio::runtime::task::spawn
//! 0x55a1b2c3d890 std::thread::spawn
//! ---
//! --- heap
//! ...
//! ```
//!
//! The JavaScript visualizer reads this symbol section to display function names
//! instead of hex addresses in flame graphs and call graphs.
//!
//! ## Symbol Resolution Process
//!
//! 1. **Extract addresses**: Parse all hex addresses from the heap dump
//! 2. **Find base address**: Locate the binary's load address from memory mappings
//! 3. **Calculate relative addresses**: Convert absolute → relative for addr2line
//! 4. **Resolve symbols**: Use addr2line's symbol table lookup
//! 5. **Demangle**: Convert mangled names (e.g., `_ZN...`) to readable Rust names
//! 6. **Append section**: Write symbol table to end of `.prof` file
//!
//! ## Graceful Degradation
//!
//! - If `addr2line` fails, falls back to hex addresses (e.g., `0x55a1b2c3d4e5`)
//! - If binary has no symbols, uses hex addresses
//! - Profiles remain valid even if symbol resolution completely fails
//!
//! ## Platform Support
//!
//! Works on any Unix system with debug symbols in the binary. Requires the
//! `addr2line` crate which reads ELF symbol tables and DWARF debug info.

use std::collections::HashMap;
use std::collections::HashSet;

use regex::Regex;

use crate::plugins::diagnostics::DiagnosticsError;
use crate::plugins::diagnostics::DiagnosticsResult;

/// Enhanced heap profile processor that embeds symbols for standalone analysis
pub(crate) struct SymbolResolver {
    binary_path: String,
    content: String,
}

impl SymbolResolver {
    /// Create a new enhanced heap processor for the given binary and heap profile
    pub(crate) async fn new(binary_path: String, input_path: &str) -> DiagnosticsResult<Self> {
        let content = std::fs::read_to_string(input_path).map_err(|e| {
            DiagnosticsError::Internal(format!("Failed to read heap profile {}: {}", input_path, e))
        })?;

        Ok(Self {
            binary_path,
            content,
        })
    }

    /// Process a raw heap profile and append symbols for in-place enhancement
    pub(crate) async fn enhance_heap_profile(&self, input_path: &str) -> DiagnosticsResult<()> {
        // Parse the heap profile to extract addresses and base address
        let (addresses, base_address) = self.extract_addresses_from_heap_profile()?;

        if addresses.is_empty() {
            return self.append_basic_symbol_section(input_path).await;
        }

        // Resolve symbols for all addresses
        let symbols = self.resolve_symbols(&addresses, base_address).await?;

        // Append symbol section to the original file
        self.append_symbol_section(input_path, &symbols).await
    }

    /// Extract all unique addresses from a heap profile and find the binary base address
    fn extract_addresses_from_heap_profile(
        &self,
    ) -> DiagnosticsResult<(HashSet<u64>, Option<u64>)> {
        let addresses = self.extract_addresses(&self.content)?;
        let base_address = self.extract_base_address(&self.content)?;

        Ok((addresses, base_address))
    }

    /// Extract all hex addresses from heap profile content
    fn extract_addresses(&self, content: &str) -> DiagnosticsResult<HashSet<u64>> {
        let mut addresses = HashSet::new();

        // Regex to find hex addresses (both 0x prefixed and raw hex)
        let hex_regex = Regex::new(r"(?m)(?:^|\s)(?:0x)?([0-9a-fA-F]+)(?:\s|$|-[0-9a-fA-F]+)")
            .expect("regex must be valid");

        for cap in hex_regex.captures_iter(content) {
            if let Ok(addr) = u64::from_str_radix(&cap[1], 16) {
                addresses.insert(addr);
            }
        }

        Ok(addresses)
    }

    /// Extract base address from mapping lines containing the binary path
    fn extract_base_address(&self, content: &str) -> DiagnosticsResult<Option<u64>> {
        // Regex to find the first hex address on lines containing our binary path
        let base_regex = Regex::new(&format!(
            r"(?m)^([0-9a-fA-F]+)-[0-9a-fA-F]+\s+.*\s+{}",
            regex::escape(&self.binary_path)
        ))
        .expect("regex must be valid");

        if let Some(cap) = base_regex.captures(content)
            && let Ok(addr) = u64::from_str_radix(&cap[1], 16)
        {
            tracing::debug!(
                "Parsed binary base address: 0x{:x} for binary: {}",
                addr,
                self.binary_path
            );
            return Ok(Some(addr));
        }

        Ok(None)
    }

    /// Resolve symbols for a set of addresses using addr2line
    async fn resolve_symbols(
        &self,
        addresses: &HashSet<u64>,
        base_address: Option<u64>,
    ) -> DiagnosticsResult<HashMap<u64, String>> {
        tracing::debug!(
            "Creating symbol table for {} addresses from: {}",
            addresses.len(),
            self.binary_path
        );

        if let Some(base) = base_address {
            tracing::debug!("Using base address 0x{:x} for symbol resolution", base);
        } else {
            tracing::warn!("No base address found, symbol resolution may not work correctly");
        }

        let mut symbols = HashMap::new();

        // Try to use addr2line for symbol resolution
        let loader_result = tokio::task::spawn_blocking({
            let binary_path = self.binary_path.clone();
            let addresses = addresses.clone();
            move || {
                // Use find_symbol API - this works for symbol table lookups
                // find_frames requires DWARF debug info which may not be available for all symbols

                // Use the simplified Loader API
                let loader = addr2line::Loader::new(&binary_path)
                    .map_err(|e| format!("Failed to create addr2line loader: {}", e))?;

                let mut resolved_symbols = HashMap::new();
                let mut symbols_resolved = 0;
                let mut symbols_failed = 0;

                for &absolute_address in &addresses {
                    // Calculate relative address if we have a base address
                    let address_for_resolution = base_address
                        // Convert absolute address to relative address
                        // Note, address is below base thus might be from a different library
                        .and_then(|base| absolute_address.checked_sub(base))
                        .unwrap_or(absolute_address);

                    // Try to resolve symbol name for this address using find_symbol
                    // This works better than find_frames for symbol table lookups
                    match loader.find_symbol(address_for_resolution) {
                        Some(symbol_name) => {
                            // Demangle the symbol name using addr2line's auto-demangling
                            let demangled_name = addr2line::demangle_auto(
                                std::borrow::Cow::Borrowed(symbol_name),
                                None, // Let it auto-detect the language
                            );

                            symbols_resolved += 1;
                            // Use the absolute address as the key for the symbol map, store demangled name
                            resolved_symbols.insert(absolute_address, demangled_name.to_string());
                        }
                        None => {
                            symbols_failed += 1;

                            // Fall back to hex address if resolution fails
                            resolved_symbols
                                .insert(absolute_address, format!("0x{:x}", absolute_address));
                        }
                    }
                }

                // Log detailed symbol resolution statistics
                let total_addresses = addresses.len();
                let success_rate = if total_addresses > 0 {
                    (symbols_resolved as f64 / total_addresses as f64) * 100.0
                } else {
                    0.0
                };

                if symbols_resolved > 0 {
                    tracing::debug!(
                        "Symbol resolution successful for {}/{} addresses ({:.1}%)",
                        symbols_resolved, total_addresses, success_rate
                    );
                } else {
                    tracing::debug!(
                        "No symbols resolved, all {}/{} addresses failed ({:.1}% success rate)",
                        symbols_failed, total_addresses, success_rate
                    );
                }

                Ok::<HashMap<u64, String>, String>(resolved_symbols)
            }
        })
        .await;

        match loader_result {
            Ok(Ok(resolved_symbols)) => {
                symbols = resolved_symbols;
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "Address resolution failed, falling back to hex addresses");
                // Fall back to hex addresses
                for &address in addresses {
                    symbols.insert(address, format!("0x{:x}", address));
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "Address resolution failed, falling back to hex addresses");
                // Fall back to hex addresses
                for &address in addresses {
                    symbols.insert(address, format!("0x{:x}", address));
                }
            }
        }

        Ok(symbols)
    }

    /// Append symbol section to the original heap profile file
    async fn append_symbol_section(
        &self,
        input_path: &str,
        symbols: &HashMap<u64, String>,
    ) -> DiagnosticsResult<()> {
        use tokio::fs::OpenOptions;
        use tokio::io::AsyncWriteExt;

        tracing::info!("Appending symbol section with {} symbols", symbols.len());

        let mut file = OpenOptions::new()
            .append(true)
            .open(input_path)
            .await
            .map_err(|e| {
                DiagnosticsError::Internal(format!("Failed to open file for appending: {}", e))
            })?;

        let mut symbol_content = String::new();

        // Write symbol section header
        symbol_content.push_str("--- symbol\n");

        // Write binary path
        symbol_content.push_str(&format!("binary={}\n", self.binary_path));

        // Write symbol entries (sorted by address for consistency)
        let mut sorted_symbols: Vec<_> = symbols.iter().collect();
        sorted_symbols.sort_by_key(|(addr, _)| **addr);

        for (&address, symbol_name) in sorted_symbols.into_iter() {
            symbol_content.push_str(&format!("0x{:x} {}\n", address, symbol_name));
        }

        // Write end of symbol section
        symbol_content.push_str("---\n");

        // Write heap section header
        symbol_content.push_str("--- heap\n");

        // Append the symbol section to the original file
        file.write_all(symbol_content.as_bytes())
            .await
            .map_err(|e| {
                DiagnosticsError::Internal(format!("Failed to append symbol section: {}", e))
            })?;

        file.flush()
            .await
            .map_err(|e| DiagnosticsError::Internal(format!("Failed to flush file: {}", e)))?;

        Ok(())
    }

    /// Append basic symbol section when no addresses are found
    async fn append_basic_symbol_section(&self, input_path: &str) -> DiagnosticsResult<()> {
        use tokio::fs::OpenOptions;
        use tokio::io::AsyncWriteExt;

        let mut file = OpenOptions::new()
            .append(true)
            .open(input_path)
            .await
            .map_err(|e| {
                DiagnosticsError::Internal(format!("Failed to open file for appending: {}", e))
            })?;

        let symbol_content = format!("--- symbol\nbinary={}\n---\n--- heap\n", self.binary_path);

        // Append the basic symbol section to the original file
        file.write_all(symbol_content.as_bytes())
            .await
            .map_err(|e| {
                DiagnosticsError::Internal(format!("Failed to append basic symbol section: {}", e))
            })?;

        file.flush()
            .await
            .map_err(|e| DiagnosticsError::Internal(format!("Failed to flush file: {}", e)))?;

        Ok(())
    }

    /// Get the binary path for the current executable
    ///
    /// SECURITY NOTE: This exposes the filesystem path of the running binary
    /// which may reveal deployment structure, usernames, or directory layouts.
    /// Only used for symbol resolution in heap profile enhancement.
    pub(crate) fn current_binary_path() -> DiagnosticsResult<String> {
        std::env::current_exe()
            .map(|p| p.to_string_lossy().to_string())
            .map_err(|e| {
                DiagnosticsError::Internal(format!("Failed to get current binary path: {}", e))
            })
    }
}

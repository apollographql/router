//! Enhanced heap profile processing with embedded symbols
//!
//! This module provides functionality to automatically resolve symbols from heap profiles
//! and create enhanced profiles that work independently of the original binary.

use std::collections::HashMap;
use std::collections::HashSet;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;

use log::debug;

use crate::plugins::diagnostics::DiagnosticsError;
use crate::plugins::diagnostics::DiagnosticsResult;

/// Enhanced heap profile processor that embeds symbols for standalone analysis
pub(crate) struct SymbolResolver {
    binary_path: String,
}

impl SymbolResolver {
    /// Create a new enhanced heap processor for the given binary
    pub(crate) fn new(binary_path: String) -> Self {
        Self { binary_path }
    }

    /// Process a raw heap profile and append symbols for in-place enhancement
    pub(crate) async fn enhance_heap_profile(&self, input_path: &str) -> DiagnosticsResult<()> {
        // Parse the heap profile to extract addresses and base address
        let (addresses, base_address) =
            self.extract_addresses_from_heap_profile(input_path).await?;

        if addresses.is_empty() {
            return self.append_basic_symbol_section(input_path).await;
        }

        // Resolve symbols for all addresses
        let symbols = self.resolve_symbols(&addresses, base_address).await?;

        // Append symbol section to the original file
        self.append_symbol_section(input_path, &symbols).await
    }

    /// Extract all unique addresses from a heap profile and find the binary base address
    async fn extract_addresses_from_heap_profile(
        &self,
        input_path: &str,
    ) -> DiagnosticsResult<(HashSet<u64>, Option<u64>)> {
        let file = File::open(input_path).map_err(|e| {
            DiagnosticsError::Internal(format!("Failed to open heap profile {}: {}", input_path, e))
        })?;

        let reader = BufReader::new(file);
        let mut addresses = HashSet::new();
        let mut base_address: Option<u64> = None;
        let mut in_stack_trace = false;
        let mut in_mapped_libraries = false;

        for line in reader.lines() {
            let line = line.map_err(|e| {
                DiagnosticsError::Internal(format!("Failed to read heap profile line: {}", e))
            })?;

            // Check for MAPPED_LIBRARIES section
            if line == "MAPPED_LIBRARIES:" {
                in_mapped_libraries = true;
                continue;
            }

            if in_mapped_libraries {
                // Look for the main binary mapping (contains our binary path)
                if line.contains(&self.binary_path) {
                    // Parse line like: 584070133000-5840703ed000 r--p 00000000 103:09 45112171  /path/to/router
                    if let Some(addr_range) = line.split_whitespace().next()
                        && let Some(start_addr_str) = addr_range.split('-').next()
                        && let Ok(addr) = u64::from_str_radix(start_addr_str, 16)
                    {
                        base_address = Some(addr);
                        tracing::debug!(
                            "Parsed binary base address: 0x{:x} from line: {}",
                            addr,
                            line
                        );
                        break; // We found what we need
                    }
                }
                continue;
            }

            // Look for stack trace lines starting with @
            if line.starts_with('@') {
                in_stack_trace = true;
                // Parse addresses from the line: @ 0x5ed51a3c46e4 0x5ed51a3c4ca5 ...
                for part in line.split_whitespace().skip(1) {
                    // Skip the '@' symbol
                    if let Some(addr_str) = part.strip_prefix("0x")
                        && let Ok(addr) = u64::from_str_radix(addr_str, 16)
                    {
                        addresses.insert(addr);
                    }
                }
            } else if in_stack_trace && line.trim().is_empty() {
                in_stack_trace = false;
            }
        }

        Ok((addresses, base_address))
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

            move || -> Result<HashMap<u64, String>, String> {
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
                    let address_for_resolution = if let Some(base) = base_address {
                        // Convert absolute address to relative address
                        if absolute_address >= base {
                            absolute_address - base
                        } else {
                            // Address is below base, might be from a different library
                            absolute_address
                        }
                    } else {
                        absolute_address
                    };

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
                    debug!(
                        "✅ Symbol resolution successful for {}/{} addresses ({:.1}%)",
                        symbols_resolved, total_addresses, success_rate
                    );
                } else {
                    debug!(
                        "❌ No symbols resolved! All {}/{} addresses failed ({:.1}% success rate)",
                        symbols_failed, total_addresses, success_rate
                    );
                }

                Ok(resolved_symbols)
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

# Changelog

## [Unreleased]

### Changed

- Refactored output handling:

  - Default to pipe output (stdout) when no output directory specified
  - File output (raw/wav) only generated when output directory provided
  - Log files only generated when explicitly enabled with --enable-logging flag
  - Changed status messages to use stderr for better pipe compatibility
  - Made output directory optional via --output-dir flag
  - Changed default format to 'raw' when saving files

- Improved user experience:
  - Added detailed help text with examples for each command option
  - Organized help into logical sections (TARGET, RECORDING, OUTPUT, DISPLAY)
  - Added clear mode indicators (pipe vs file output)
  - Improved process selection with sorting and better filtering feedback
  - Added example commands for common use cases
  - Added pipe command examples with correct audio parameters
  - Better status messages with emojis and formatting
  - Clearer error messages with more context
  - Added terminal detection to prevent raw audio output to console
  - Added helpful suggestions when attempting to output to terminal

### Added

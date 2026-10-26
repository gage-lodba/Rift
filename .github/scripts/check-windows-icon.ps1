#requires -Version 7
<#
.SYNOPSIS
    Fail a Windows build when the executable's file icon is missing from its PE
    resources.

.DESCRIPTION
    Explorer, the Start menu, shortcuts and a pinned taskbar entry draw an
    executable's icon from resources *inside the file*; the shell never starts
    the program to ask it. Those resources are RT_GROUP_ICON (resource type 14,
    the per-resolution directory the shell reads first) and RT_ICON (type 3, the
    actual image blobs). An image with neither gets the generic Windows
    application icon no matter what the program draws in its own window.

    The file icon and the runtime window icon are different things. The app sets
    its window icon from `assets/icons/128x128.png` when it creates the window
    (`window_icon()` in `crates/rift-desktop/src/main.rs`); that only affects the
    live window and taskbar button and cannot make up for a missing file
    resource. Explorer still shows the generic icon.

    This guard exists because the failure is silent: losing the resource breaks
    no compile, link, or test. `cargo build` succeeds, the app runs fine, and
    only Explorer shows the wrong icon. The previous desktop shell embedded the
    file icon in its build step; the native `cargo build` rewrite replaced that
    step without replacing the embedded resource, and nothing failed. The icon
    is re-embedded by `crates/rift-desktop/build.rs` (a Windows-only
    `embed-resource` build dependency compiling `assets/icons/rift.rc`); this
    script is the check that it stayed embedded.

    The check parses the PE itself: DOS header -> e_lfanew -> `PE\0\0` signature
    -> COFF header -> optional-header magic (PE32/PE32+) -> data directory 2
    (the resource table) -> RVA-to-file-offset through the section table -> the
    resource directory tree. Every read is bounds-checked, so a truncated or
    corrupt image fails with a descriptive error rather than an index-out-of-
    range exception.

    Deliberately no `dumpbin` (only present with a Visual Studio developer
    prompt and its output format changes between versions), no `System.Drawing`
    / `Icon.ExtractAssociatedIcon` (GDI+ is an optional CI dependency and would
    answer the shell's question instead of the file's), and no modules: only
    `[System.IO.File]::ReadAllBytes` plus byte arithmetic, so the script itself
    is safe to run on any host (CI invokes it on Windows, where the resource
    matters).

    Exit codes: 0 when the image has an RT_GROUP_ICON resource and at least one
    non-empty RT_ICON image; 1 for a missing icon resource, an unreadable,
    non-PE or truncated file, or a malformed resource tree. Failures print
    GitHub Actions `::error::` annotations so CI surfaces the file and the fix.

.PARAMETER Path
    The PE image to inspect (normally `target/release/rift-music.exe`).
    Positional and pipeline input are accepted; a piped FileInfo resolves
    through its FullName.

.EXAMPLE
    pwsh .github/scripts/check-windows-icon.ps1 -Path target/release/rift-music.exe

.EXAMPLE
    Get-Item target/release/rift-music.exe | ./.github/scripts/check-windows-icon.ps1
#>
[CmdletBinding()]
param(
    # [object] rather than [string]: casting a piped FileInfo to string yields
    # only the file name, which would silently check the wrong file. The
    # FullName alias makes the pipeline-by-property-name path bind too.
    [Parameter(Mandatory, Position = 0, ValueFromPipeline, ValueFromPipelineByPropertyName)]
    [Alias('FullName')]
    [object]$Path
)

begin {
    $ErrorActionPreference = 'Stop'
    Set-StrictMode -Version Latest

    # Windows resource type ids from winuser.h. An .rc file's
    # `1 ICON "icon.ico"` line produces exactly these two predefined numeric
    # types, so the guard looks for the ids, never for a name string.
    $script:RtIcon = 3
    $script:RtGroupIcon = 14

    # ------------------------------------------------------------------
    # Bounds-checked little-endian readers.
    #
    # PE is little-endian. [BitConverter] uses the *host's* byte order, so the
    # bytes are combined by hand here. Every read takes an absolute file offset
    # and a context label; a corrupt image fails with "cannot read <what> at
    # offset N" instead of a PowerShell index-out-of-range error.
    # ------------------------------------------------------------------
    function Read-LeUInt16 {
        param(
            [Parameter(Mandatory)][byte[]]$Bytes,
            [Parameter(Mandatory)][long]$Offset,
            [Parameter(Mandatory)][string]$Context
        )
        if ($Offset -lt 0 -or ($Offset + 2) -gt $Bytes.Length) {
            throw ("truncated PE: cannot read {0} (2 bytes at offset {1}; the file is {2} bytes)" -f $Context, $Offset, $Bytes.Length)
        }
        $base = [int]$Offset
        return ([long]$Bytes[$base]) -bor ([long]$Bytes[$base + 1] -shl 8)
    }

    function Read-LeUInt32 {
        param(
            [Parameter(Mandatory)][byte[]]$Bytes,
            [Parameter(Mandatory)][long]$Offset,
            [Parameter(Mandatory)][string]$Context
        )
        if ($Offset -lt 0 -or ($Offset + 4) -gt $Bytes.Length) {
            throw ("truncated PE: cannot read {0} (4 bytes at offset {1}; the file is {2} bytes)" -f $Context, $Offset, $Bytes.Length)
        }
        $base = [int]$Offset
        return ([long]$Bytes[$base]) -bor
        ([long]$Bytes[$base + 1] -shl 8) -bor
        ([long]$Bytes[$base + 2] -shl 16) -bor
        ([long]$Bytes[$base + 3] -shl 24)
    }

    # Map a PE relative virtual address to a file offset through the section
    # table. Throws for an RVA that no section maps, or that lands past the
    # section's raw data (uninitialized padding has no bytes in the file).
    function Resolve-PeRvaToOffset {
        param(
            [Parameter(Mandatory)][long]$Rva,
            [Parameter(Mandatory)][object[]]$Sections,
            [Parameter(Mandatory)][long]$FileLength,
            [string]$Context = 'RVA'
        )
        foreach ($section in $Sections) {
            # VirtualSize is the mapped size; some linkers leave it 0 on
            # data-only sections, so the larger of the two decides membership.
            $mappedSize = [Math]::Max([long]$section.VirtualSize, [long]$section.RawSize)
            if ($Rva -lt $section.VirtualAddress -or $Rva -ge ($section.VirtualAddress + $mappedSize)) {
                continue
            }
            $delta = $Rva - $section.VirtualAddress
            if ($delta -ge $section.RawSize) {
                throw ("corrupt PE: {0} RVA 0x{1:X} lies in section '{2}' past its {3} bytes of raw data" -f $Context, $Rva, $section.Name, $section.RawSize)
            }
            $fileOffset = [long]$section.RawPointer + $delta
            if ($fileOffset -ge $FileLength) {
                throw ("corrupt PE: {0} RVA 0x{1:X} maps to file offset {2}, past the end of the {3}-byte file" -f $Context, $Rva, $fileOffset, $FileLength)
            }
            return $fileOffset
        }
        throw ("corrupt PE: {0} RVA 0x{1:X} is not mapped by any section" -f $Context, $Rva)
    }

    # Read an IMAGE_RESOURCE_DIRECTORY header (16 bytes) and check that it and
    # its entry array sit inside the resource directory window.
    function Read-PeResourceDirectoryHeader {
        param(
            [Parameter(Mandatory)][byte[]]$Bytes,
            [Parameter(Mandatory)][long]$DirectoryOffset,
            [Parameter(Mandatory)][long]$ResourceBase,
            [Parameter(Mandatory)][long]$ResourceEnd,
            [Parameter(Mandatory)][string]$Context
        )
        if ($DirectoryOffset -lt $ResourceBase -or ($DirectoryOffset + 16) -gt $ResourceEnd) {
            throw ("corrupt PE: the {0} directory header at file offset {1} is outside the resource directory (file offsets {2}..{3})" -f $Context, $DirectoryOffset, $ResourceBase, $ResourceEnd)
        }
        $namedCount = Read-LeUInt16 -Bytes $Bytes -Offset ($DirectoryOffset + 12) -Context "$Context named-entry count"
        $idCount = Read-LeUInt16 -Bytes $Bytes -Offset ($DirectoryOffset + 14) -Context "$Context id-entry count"
        return [pscustomobject]@{
            NamedCount  = $namedCount
            IdCount     = $idCount
            EntryCount  = ([long]$namedCount + [long]$idCount)
            EntryOffset = ($DirectoryOffset + 16)
        }
    }

    # Walk one resource-directory subtree and return every
    # IMAGE_RESOURCE_DATA_ENTRY below it as { DataRva, Size }. Offsets in the
    # tree are relative to the resource base and have the high bit as a
    # "points to subdirectory" flag.
    function Get-PeResourceLeafEntries {
        param(
            [Parameter(Mandatory)][byte[]]$Bytes,
            [Parameter(Mandatory)][long]$DirectoryOffset,
            [Parameter(Mandatory)][long]$ResourceBase,
            [Parameter(Mandatory)][long]$ResourceEnd,
            [Parameter(Mandatory)][string]$Context,
            [int]$Depth = 0
        )
        # A well-formed tree is root -> type -> name -> language: three
        # directory levels before the data entries. The cap turns a cyclic or
        # absurdly nested (corrupt) tree into a clear failure instead of
        # unbounded recursion.
        if ($Depth -gt 8) {
            throw ("corrupt PE: the {0} resource directory is nested deeper than 8 levels; the tree is cyclic or invalid" -f $Context)
        }
        $header = Read-PeResourceDirectoryHeader -Bytes $Bytes -DirectoryOffset $DirectoryOffset -ResourceBase $ResourceBase -ResourceEnd $ResourceEnd -Context $Context
        $entriesEnd = $header.EntryOffset + ($header.EntryCount * 8)
        if ($entriesEnd -gt $ResourceEnd) {
            throw ("corrupt PE: the {0} resource directory declares {1} entries that run past the end of the resource directory" -f $Context, $header.EntryCount)
        }

        $leaves = [System.Collections.Generic.List[object]]::new()
        for ($index = 0; $index -lt $header.EntryCount; $index++) {
            $entryOffset = $header.EntryOffset + ($index * 8)
            $nameField = Read-LeUInt32 -Bytes $Bytes -Offset $entryOffset -Context "$Context entry name"
            $offsetField = Read-LeUInt32 -Bytes $Bytes -Offset ($entryOffset + 4) -Context "$Context entry offset"
            $entryLabel = if (($nameField -band 0x80000000) -ne 0) {
                'named entry'
            } else {
                'entry id {0}' -f $nameField
            }
            $childLabel = "$Context/$entryLabel"
            if (($offsetField -band 0x80000000) -ne 0) {
                # High bit set: OffsetToData is a subdirectory offset relative
                # to the resource base. Recurse.
                $childOffset = $ResourceBase + [long]($offsetField -band 0x7FFFFFFF)
                $childLeaves = @(Get-PeResourceLeafEntries -Bytes $Bytes -DirectoryOffset $childOffset -ResourceBase $ResourceBase -ResourceEnd $ResourceEnd -Context $childLabel -Depth ($Depth + 1))
                foreach ($leaf in $childLeaves) { $leaves.Add($leaf) }
            } else {
                # High bit clear: OffsetToData is the offset of an
                # IMAGE_RESOURCE_DATA_ENTRY (16 bytes: DataRva, Size,
                # CodePage, Reserved) relative to the resource base.
                $dataEntryOffset = $ResourceBase + [long]($offsetField -band 0x7FFFFFFF)
                if ($dataEntryOffset -lt $ResourceBase -or ($dataEntryOffset + 16) -gt $ResourceEnd) {
                    throw ("corrupt PE: the {0} data entry at file offset {1} is outside the resource directory" -f $childLabel, $dataEntryOffset)
                }
                $leaves.Add([pscustomobject]@{
                        DataRva = Read-LeUInt32 -Bytes $Bytes -Offset $dataEntryOffset -Context "$childLabel data RVA"
                        Size    = Read-LeUInt32 -Bytes $Bytes -Offset ($dataEntryOffset + 4) -Context "$childLabel data size"
                    })
            }
        }
        return $leaves
    }

    # A non-empty resource's blob RVA must resolve to bytes that are actually
    # in the file. Empty entries are allowed here (the caller treats an empty
    # RT_ICON as a missing image), so they are not resolved.
    function Test-PeResourceDataBounds {
        param(
            [Parameter(Mandatory)][byte[]]$Bytes,
            [Parameter(Mandatory)][object[]]$Sections,
            [Parameter(Mandatory)][long]$DataRva,
            [Parameter(Mandatory)][long]$DataSize,
            [Parameter(Mandatory)][string]$Description
        )
        if ($DataSize -le 0) { return }
        $fileOffset = Resolve-PeRvaToOffset -Rva $DataRva -Sections $Sections -FileLength $Bytes.Length -Context $Description
        if (($fileOffset + $DataSize) -gt $Bytes.Length) {
            throw ("corrupt PE: {0} is {1} bytes at RVA 0x{2:X} (file offset {3}), which runs past the end of the {4}-byte file" -f $Description, $DataSize, $DataRva, $fileOffset, $Bytes.Length)
        }
    }

    # ------------------------------------------------------------------
    # The PE parse itself.
    # ------------------------------------------------------------------
    function Get-PeIconResourceReport {
        param([Parameter(Mandatory)][string]$BinaryPath)

        $bytes = [System.IO.File]::ReadAllBytes($BinaryPath)

        # DOS header. Only 'MZ' and e_lfanew are needed; the rest of the
        # stub is irrelevant to resource lookup.
        if ($bytes.Length -lt 0x40) {
            throw ("file is only {0} bytes, too small to hold a DOS header" -f $bytes.Length)
        }
        if (($bytes[0] -ne 0x4D) -or ($bytes[1] -ne 0x5A)) {
            throw ("not a PE image: no 'MZ' signature at offset 0 (found 0x{0:X2} 0x{1:X2})" -f $bytes[0], $bytes[1])
        }
        $eLfanew = Read-LeUInt32 -Bytes $bytes -Offset 0x3C -Context 'DOS header e_lfanew'
        if (($eLfanew + 4 + 20) -gt $bytes.Length) {
            throw ("truncated PE: e_lfanew points to offset {0}, outside this {1}-byte file" -f $eLfanew, $bytes.Length)
        }
        $signature = Read-LeUInt32 -Bytes $bytes -Offset $eLfanew -Context 'PE signature'
        if ($signature -ne 0x00004550) {
            throw ("not a PE image: expected the 'PE\0\0' signature at offset {0}, found 0x{1:X8}" -f $eLfanew, $signature)
        }

        # COFF file header: section count and the size of the optional header
        # that follows it. The section table lives after the optional header.
        $coffOffset = $eLfanew + 4
        $sectionCount = Read-LeUInt16 -Bytes $bytes -Offset ($coffOffset + 2) -Context 'COFF section count'
        $sizeOfOptionalHeader = Read-LeUInt16 -Bytes $bytes -Offset ($coffOffset + 16) -Context 'COFF size of optional header'
        $optionalHeaderOffset = $coffOffset + 20
        if ($sizeOfOptionalHeader -lt 2) {
            throw ("corrupt PE: SizeOfOptionalHeader is {0}, too small to hold the optional header magic" -f $sizeOfOptionalHeader)
        }
        if (($optionalHeaderOffset + $sizeOfOptionalHeader) -gt $bytes.Length) {
            throw ("truncated PE: the {0}-byte optional header at offset {1} runs past the end of the file" -f $sizeOfOptionalHeader, $optionalHeaderOffset)
        }

        # Optional-header layout differs between PE32 (32-bit) and PE32+
        # (64-bit): magic 0x10B / 0x20B, with the data-directory array at
        # offset 96 / 112 from the start of the optional header.
        $magic = Read-LeUInt16 -Bytes $bytes -Offset $optionalHeaderOffset -Context 'optional header magic'
        switch ($magic) {
            0x10B {
                $peFormat = 'PE32'
                $numberOfRvaAndSizesOffset = 92
                $dataDirectoryOffset = 96
            }
            0x20B {
                $peFormat = 'PE32+'
                $numberOfRvaAndSizesOffset = 108
                $dataDirectoryOffset = 112
            }
            default {
                throw ("corrupt or unsupported PE: optional header magic 0x{0:X3} is neither PE32 (0x10B) nor PE32+ (0x20B)" -f $magic)
            }
        }
        if ($sizeOfOptionalHeader -lt ($numberOfRvaAndSizesOffset + 4)) {
            throw ("corrupt PE: SizeOfOptionalHeader is {0}, too small for the NumberOfRvaAndSizes field at offset {1}" -f $sizeOfOptionalHeader, $numberOfRvaAndSizesOffset)
        }
        $numberOfRvaAndSizes = Read-LeUInt32 -Bytes $bytes -Offset ($optionalHeaderOffset + $numberOfRvaAndSizesOffset) -Context 'NumberOfRvaAndSizes'
        if ($numberOfRvaAndSizes -gt 16) {
            throw ("corrupt PE: NumberOfRvaAndSizes is {0}; the PE format defines at most 16 data directories" -f $numberOfRvaAndSizes)
        }

        # Section table, used only for RVA -> file-offset translation.
        if ($sectionCount -eq 0) {
            throw 'corrupt PE: the section count is 0, so no RVA can be mapped to a file offset'
        }
        $sectionTableOffset = $optionalHeaderOffset + $sizeOfOptionalHeader
        $sectionTableEnd = $sectionTableOffset + ([long]$sectionCount * 40)
        if ($sectionTableEnd -gt $bytes.Length) {
            throw ("truncated PE: the section table ({0} entries) runs past the end of the file" -f $sectionCount)
        }
        $sections = [System.Collections.Generic.List[object]]::new()
        for ($index = 0; $index -lt $sectionCount; $index++) {
            $sectionOffset = $sectionTableOffset + ($index * 40)
            $name = [System.Text.Encoding]::ASCII.GetString($bytes, [int]$sectionOffset, 8).TrimEnd([char]0)
            if ([string]::IsNullOrWhiteSpace($name)) {
                $name = '<unnamed section {0}>' -f $index
            }
            $sections.Add([pscustomobject]@{
                    Name           = $name
                    VirtualSize    = Read-LeUInt32 -Bytes $bytes -Offset ($sectionOffset + 8) -Context "section $index virtual size"
                    VirtualAddress = Read-LeUInt32 -Bytes $bytes -Offset ($sectionOffset + 12) -Context "section $index virtual address"
                    RawSize        = Read-LeUInt32 -Bytes $bytes -Offset ($sectionOffset + 16) -Context "section $index raw size"
                    RawPointer     = Read-LeUInt32 -Bytes $bytes -Offset ($sectionOffset + 20) -Context "section $index raw pointer"
                })
        }
        $sectionArray = $sections.ToArray()

        # Data directory 2 is the resource table. PEs that stop before it have
        # no resources at all; a zero RVA/size means the same.
        $hasResourceDirectory = $false
        $resourceTypeIds = [System.Collections.Generic.List[long]]::new()
        $groupLeaves = [System.Collections.Generic.List[object]]::new()
        $iconLeaves = [System.Collections.Generic.List[object]]::new()

        if ($numberOfRvaAndSizes -gt 2) {
            if (($dataDirectoryOffset + 24) -gt $sizeOfOptionalHeader) {
                throw ("corrupt PE: SizeOfOptionalHeader is {0}, too small to hold data directory 2 (needs {1})" -f $sizeOfOptionalHeader, ($dataDirectoryOffset + 24))
            }
            $resourceRva = Read-LeUInt32 -Bytes $bytes -Offset ($optionalHeaderOffset + $dataDirectoryOffset + 16) -Context 'resource data directory RVA'
            $resourceSize = Read-LeUInt32 -Bytes $bytes -Offset ($optionalHeaderOffset + $dataDirectoryOffset + 20) -Context 'resource data directory size'
            if ($resourceRva -ne 0 -and $resourceSize -ne 0) {
                $hasResourceDirectory = $true
                $resourceStart = Resolve-PeRvaToOffset -Rva $resourceRva -Sections $sectionArray -FileLength $bytes.Length -Context 'resource directory'
                if (($resourceStart + $resourceSize) -gt $bytes.Length) {
                    throw ("corrupt PE: the {0}-byte resource directory at file offset {1} runs past the end of the {2}-byte file" -f $resourceSize, $resourceStart, $bytes.Length)
                }
                $resourceEnd = $resourceStart + $resourceSize

                # Level 1 of the tree: resource types. Icon types are
                # predefined integer ids (high bit clear); named type entries
                # are strings and never the predefined icon types.
                $root = Read-PeResourceDirectoryHeader -Bytes $bytes -DirectoryOffset $resourceStart -ResourceBase $resourceStart -ResourceEnd $resourceEnd -Context 'root'
                for ($index = 0; $index -lt $root.EntryCount; $index++) {
                    $entryOffset = $root.EntryOffset + ($index * 8)
                    $nameField = Read-LeUInt32 -Bytes $bytes -Offset $entryOffset -Context 'root resource entry name'
                    $offsetField = Read-LeUInt32 -Bytes $bytes -Offset ($entryOffset + 4) -Context 'root resource entry offset'
                    if (($nameField -band 0x80000000) -ne 0) { continue }
                    $typeId = [long]$nameField
                    $resourceTypeIds.Add($typeId)
                    if ($typeId -ne $script:RtIcon -and $typeId -ne $script:RtGroupIcon) { continue }
                    if (($offsetField -band 0x80000000) -eq 0) {
                        throw ("corrupt PE: top-level resource type {0} points straight at a data entry; the type level must be a directory" -f $typeId)
                    }
                    $typeDirectoryOffset = $resourceStart + [long]($offsetField -band 0x7FFFFFFF)
                    $typeName = if ($typeId -eq $script:RtGroupIcon) { 'RT_GROUP_ICON (type 14)' } else { 'RT_ICON (type 3)' }
                    $leaves = @(Get-PeResourceLeafEntries -Bytes $bytes -DirectoryOffset $typeDirectoryOffset -ResourceBase $resourceStart -ResourceEnd $resourceEnd -Context ("{0} resources" -f $typeName) -Depth 1)
                    if ($typeId -eq $script:RtGroupIcon) {
                        foreach ($leaf in $leaves) { $groupLeaves.Add($leaf) }
                    } else {
                        foreach ($leaf in $leaves) { $iconLeaves.Add($leaf) }
                    }
                }
            }
        }

        # Validate the blobs we counted: a resource entry whose size is
        # non-zero must have its bytes in the file.
        # The description deliberately omits the RVA: the resolver's own error
        # messages already name it, and Test-PeResourceDataBounds reports its
        # DataRva/offset/size, so repeating it here would only read badly.
        foreach ($leaf in $groupLeaves) {
            Test-PeResourceDataBounds -Bytes $bytes -Sections $sectionArray -DataRva $leaf.DataRva -DataSize $leaf.Size -Description 'RT_GROUP_ICON resource'
        }
        foreach ($leaf in $iconLeaves) {
            Test-PeResourceDataBounds -Bytes $bytes -Sections $sectionArray -DataRva $leaf.DataRva -DataSize $leaf.Size -Description 'RT_ICON resource'
        }

        $groupBytes = [long]0
        foreach ($leaf in $groupLeaves) { $groupBytes += [long]$leaf.Size }
        $nonEmptyIconCount = 0
        $largestIconBytes = [long]0
        foreach ($leaf in $iconLeaves) {
            if ($leaf.Size -le 0) { continue }
            $nonEmptyIconCount++
            if ($leaf.Size -gt $largestIconBytes) { $largestIconBytes = [long]$leaf.Size }
        }

        return [pscustomobject]@{
            Path                = $BinaryPath
            FileSizeBytes       = [long]$bytes.Length
            PeFormat            = $peFormat
            SectionCount        = [long]$sectionCount
            HasResourceDirectory = $hasResourceDirectory
            ResourceTypeIds     = $resourceTypeIds.ToArray()
            GroupIconCount      = [long]$groupLeaves.Count
            GroupIconBytes      = $groupBytes
            IconCount           = [long]$iconLeaves.Count
            NonEmptyIconCount   = [long]$nonEmptyIconCount
            LargestIconBytes    = $largestIconBytes
        }
    }

    # ------------------------------------------------------------------
    # Turn a report into PASS/FAIL output lines plus the exit decision.
    # Returns { Passed, Lines } so process{} can print and count; never
    # writes to the output stream itself (its return value is the object).
    # ------------------------------------------------------------------
    function Get-WindowsIconCheck {
        param(
            [Parameter(Mandatory)][AllowEmptyString()][string]$BinaryPath
        )
        $lines = [System.Collections.Generic.List[string]]::new()

        if ([string]::IsNullOrWhiteSpace($BinaryPath)) {
            $lines.Add('::error::no path was supplied; pass -Path <exe> (or pipe a path or FileInfo).')
            return [pscustomobject]@{ Passed = $false; Lines = @($lines) }
        }

        try {
            if (-not (Test-Path -LiteralPath $BinaryPath -PathType Leaf)) {
                throw "file not found (or not a regular file): $BinaryPath"
            }
            $resolvedPath = (Resolve-Path -LiteralPath $BinaryPath -ErrorAction Stop).ProviderPath
            $report = Get-PeIconResourceReport -BinaryPath $resolvedPath
        } catch {
            $reason = $_.Exception.Message
            $lines.Add("FAIL: $BinaryPath")
            $lines.Add("::error::$BinaryPath could not be inspected as a PE image: $reason")
            $lines.Add('::error::the guard reads the PE headers and resource directory directly, so an unreadable, non-PE or truncated file is a failure rather than a skip.')
            return [pscustomobject]@{ Passed = $false; Lines = @($lines) }
        }

        if (-not $report.HasResourceDirectory) {
            $lines.Add("FAIL: $resolvedPath")
            $lines.Add("::error::$resolvedPath has no PE resource directory at all, so it cannot carry a Windows file icon.")
            $lines.Add("::error::Explorer, the Start menu, shortcuts and pinned taskbar entries read the icon from the executable's RT_GROUP_ICON/RT_ICON resources - they never start the program. The runtime window icon set in crates/rift-desktop/src/main.rs only affects the live window and cannot replace the file resource.")
            $lines.Add('::error::the app icon is embedded as a Windows PE resource by the Windows-only embed-resource build dependency: crates/rift-desktop/build.rs compiles crates/rift-desktop/assets/icons/rift.rc, whose `1 ICON "icon.ico"` line carries the RT_GROUP_ICON/RT_ICON pair.')
            return [pscustomobject]@{ Passed = $false; Lines = @($lines) }
        }

        if ($report.GroupIconCount -le 0) {
            $typeList = if ($report.ResourceTypeIds.Count -gt 0) { $report.ResourceTypeIds -join ', ' } else { 'none' }
            $lines.Add("FAIL: $resolvedPath")
            $lines.Add(('::error::{0} has a resource directory but no RT_GROUP_ICON resource (type 14). Resource types present: {1}.' -f $resolvedPath, $typeList))
            $lines.Add('::error::the shell reads RT_GROUP_ICON to learn which RT_ICON image is which size; a file with images but no group (or neither) still shows the generic Windows icon.')
            $lines.Add('::error::the icon is embedded by crates/rift-desktop/build.rs from crates/rift-desktop/assets/icons/rift.rc; check that the resource compiler ran and that the .rc still lists `1 ICON "icon.ico"`.')
            return [pscustomobject]@{ Passed = $false; Lines = @($lines) }
        }

        if ($report.NonEmptyIconCount -le 0) {
            $lines.Add("FAIL: $resolvedPath")
            $lines.Add(('::error::{0} has {1} RT_GROUP_ICON resource(s) but no non-empty RT_ICON (type 3) image data: the group indexes no usable image.' -f $resolvedPath, $report.GroupIconCount))
            $lines.Add('::error::check that crates/rift-desktop/assets/icons/icon.ico is non-empty and still contains the expected icon sizes; an empty or unreadable .ico compiles to an empty icon resource.')
            return [pscustomobject]@{ Passed = $false; Lines = @($lines) }
        }

        $lines.Add(('PASS: {0} - {1} image carries {2} RT_GROUP_ICON resource(s) and {3} non-empty RT_ICON image(s) (largest {4:N0} bytes).' -f $resolvedPath, $report.PeFormat, $report.GroupIconCount, $report.NonEmptyIconCount, $report.LargestIconBytes))
        return [pscustomobject]@{ Passed = $true; Lines = @($lines) }
    }

    $script:failedCount = 0
}

process {
    $candidate = if ($Path -is [System.IO.FileSystemInfo]) { $Path.FullName } else { [string]$Path }
    $check = Get-WindowsIconCheck -BinaryPath $candidate
    foreach ($line in $check.Lines) { Write-Output $line }
    if (-not $check.Passed) { $script:failedCount++ }
}

end {
    if ($script:failedCount -gt 0) {
        exit 1
    }
    exit 0
}

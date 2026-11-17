@{
    RootModule           = 'MeridianTestDsc.psm1'
    ModuleVersion        = '1.0.0'
    GUID                 = '6f0f7c4e-2a43-4d0b-9a55-0d3b6d6c7e11'
    Author               = 'Meridian'
    CompanyName          = 'Meridian'
    Copyright            = '(c) The Meridian authors.'
    Description          = 'A test-only DSC resource for the real-tool tests of the Meridian DSC plugin: a marker file whose presence and content are the state.'
    PowerShellVersion    = '7.2'
    FunctionsToExport    = @()
    VariablesToExport    = @()
    # CmdletsToExport and AliasesToExport are left out on purpose. When a manifest names
    # its functions, cmdlets and aliases without a wildcard, Get-Module -ListAvailable
    # does not look inside the module and reports no DSC resources for it, so
    # Get-DscResource and Invoke-DscResource (DSC v2) cannot find a class-based resource.
    DscResourcesToExport = @('MarkerFile')
}

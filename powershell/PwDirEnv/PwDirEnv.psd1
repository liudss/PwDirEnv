@{
    RootModule = 'PwDirEnv.psm1'
    ModuleVersion = '0.1.0'
    GUID = 'd7d52f2c-6fc4-4584-a2a9-0a7b72294e3d'
    Author = 'PwDirEnv contributors'
    Description = 'Directory environment manager for PowerShell (direnv equivalent). .envrc.ps1 runs in an isolated evaluator subprocess; only the env diff is applied to the host session.'
    PowerShellVersion = '7.2'
    CompatiblePSEditions = @('Core')
    FunctionsToExport = @(
        'Enable-PwDirEnv',
        'Disable-PwDirEnv',
        'Invoke-PwDirEnvHook',
        'Get-PwDirEnvStatus'
    )
    FileList = @(
        'PwDirEnv.psd1',
        'PwDirEnv.psm1'
    )
    PrivateData = @{
        PSData = @{
            Tags = @('PwDirEnv', 'direnv', 'environment', 'env', 'Windows', 'PowerShell', 'dotenv')
            LicenseUri = 'https://example.invalid/PwDirEnv/LICENSE'
            ProjectUri = 'https://example.invalid/PwDirEnv'
            ReleaseNotes = 'M0 scaffold: module skeleton with status introspection.'
        }
    }
}

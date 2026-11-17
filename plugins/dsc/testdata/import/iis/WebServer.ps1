<#
    A web server: IIS, the site's directory and configuration, and the
    service that serves it.
#>
Configuration WebServer {
    Import-DscResource -ModuleName PSDscResources
    Import-DscResource -ModuleName @{ ModuleName = 'WebAdministrationDsc'; ModuleVersion = '4.1.0' }

    Node 'web01.example.com' {
        WindowsFeature IIS {
            Ensure = 'Present'
            Name   = 'Web-Server'
        }

        File SiteRoot {
            Ensure          = 'Present'
            Type            = 'Directory'
            DestinationPath = 'C:\inetpub\app'
            DependsOn       = '[WindowsFeature]IIS'
        }

        File SiteConfig {
            Ensure          = 'Present'
            DestinationPath = 'C:\inetpub\app\web.config'
            Contents        = @'
<configuration>
  <appSettings><add key="price" value="$5" /></appSettings>
</configuration>
'@
            DependsOn       = '[File]SiteRoot'
        }

        WebAppPool AppPool {
            Ensure                = 'Present'
            Name                  = 'app'
            State                 = "Started"
            restartMemoryLimit    = 512mb
            autoStart             = $true
            DependsOn             = '[WindowsFeature]IIS', '[File]SiteRoot'
        }

        Service W3SVC {
            Name        = 'W3SVC'
            State       = 'Running'
            StartupType = 'Automatic'
        }
    }
}

WebServer -OutputPath .\mof
Start-DscConfiguration -Path .\mof -Wait

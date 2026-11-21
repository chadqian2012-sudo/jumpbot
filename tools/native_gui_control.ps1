param(
    [Parameter(Mandatory)][int]$ProcessId,
    [Parameter(Mandatory)][string]$GuiPath,
    [Parameter(Mandatory)][ValidateSet('Invoke', 'Phase', 'SelectBackend')][string]$Action,
    [Parameter(Mandatory)][string]$Id,
    [Parameter(Mandatory)][string]$Expected
)

$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$gui = Get-Process -Id $ProcessId
if ($gui.Path -ne $GuiPath) { throw "Unexpected GUI executable for PID $ProcessId ($($gui.Path))" }
$descendants = [Windows.Automation.TreeScope]::Descendants
$idCondition = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty, $Id)
$nameCondition = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty, $Expected)
$deadline = [Diagnostics.Stopwatch]::StartNew()

function Wait-Control([scriptblock]$Find, [string]$Description) {
    do {
        $gui.Refresh()
        if ($gui.HasExited) { throw "GUI PID $ProcessId exited ($($gui.ExitCode)) while waiting for $Description" }
        if ($gui.MainWindowHandle -ne [IntPtr]::Zero) {
            try {
                $root = [Windows.Automation.AutomationElement]::FromHandle($gui.MainWindowHandle)
                $control = $root.FindFirst($descendants, $idCondition)
                if ($control) {
                    $observed = "name='$($control.Current.Name)' enabled=$($control.Current.IsEnabled)"
                    $found = & $Find $control $root
                    if ($found) { return $found }
                }
            } catch [Windows.Automation.ElementNotAvailableException] {
                # The provider can replace a node while the page is rendering.
            }
        }
        Start-Sleep -Milliseconds 120
    } while ($deadline.Elapsed.TotalSeconds -lt 25)
    throw "UI Automation timed out for $Description, id='$Id' in '$($gui.MainWindowTitle)' PID $ProcessId. Last control: $observed"
}

switch ($Action) {
    'Invoke' {
        $button = Wait-Control { param($c)
            if ($c.Current.ControlType -eq [Windows.Automation.ControlType]::Button -and
                $c.Current.IsEnabled -and $c.Current.Name -ceq $Expected) { $c }
        } "enabled button '$Expected'"
        $button.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
    }
    'Phase' {
        $label = Wait-Control { param($c)
            if ($c.Current.Name -ceq $Expected) { return $c }
            $c.FindFirst($descendants, $nameCondition)
        } "rendered status '$Expected'"
        $label.Current.Name
    }
    'SelectBackend' {
        $combo = Wait-Control { param($c)
            if ($c.Current.ControlType -eq [Windows.Automation.ControlType]::ComboBox -and $c.Current.IsEnabled) { $c }
        } 'backend selector'
        $combo.GetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern).Expand()
        $item = Wait-Control { param($c, $root)
            $option = $root.FindFirst($descendants, $nameCondition)
            if ($option -and $option.Current.ControlType -eq [Windows.Automation.ControlType]::ListItem -and $option.Current.IsEnabled) { $option }
        } "backend option '$Expected'"
        $item.GetCurrentPattern([Windows.Automation.InvokePattern]::Pattern).Invoke()
        [void](Wait-Control { param($c)
            if ($c.GetCurrentPattern([Windows.Automation.ValuePattern]::Pattern).Current.Value -ceq $Expected) { $c }
        } "selected backend '$Expected'")
    }
}

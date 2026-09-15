param([int]$TargetProcessId, [ValidateSet('read', 'place', 'close', 'quit', 'minimize', 'maximize', 'restore')][string]$Action = 'read', [int]$X = 70, [int]$Y = 90, [int]$Width = 1320, [int]$Height = 820)
$ErrorActionPreference = 'Stop'
# Operate only on the main window belonging to the isolated QA process.
Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class LayoutWindow {
  public delegate bool EnumProc(IntPtr hwnd, IntPtr arg);
  [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left, Top, Right, Bottom; }
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc callback, IntPtr arg);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr hwnd, StringBuilder text, int size);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hwnd, out Rect rect);
  [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr hwnd, out Rect rect);
  [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr hwnd, int x, int y, int width, int height, bool repaint);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr hwnd, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hwnd, int command);
  [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr hwnd);
  [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hwnd);
  [DllImport("user32.dll")] public static extern IntPtr GetMenu(IntPtr hwnd);
  [DllImport("user32.dll")] public static extern IntPtr GetSubMenu(IntPtr menu, int index);
  [DllImport("user32.dll")] public static extern int GetMenuItemCount(IntPtr menu);
  [DllImport("user32.dll")] public static extern uint GetMenuItemID(IntPtr menu, int index);
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetMenuString(IntPtr menu, uint item, StringBuilder text, int size, uint flags);
  [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
  public static IntPtr Find(uint target) {
    IntPtr result = IntPtr.Zero;
    EnumWindows((hwnd, arg) => { uint pid; GetWindowThreadProcessId(hwnd, out pid);
      var title = new StringBuilder(256); GetWindowText(hwnd, title, title.Capacity);
      if (pid == target && title.ToString() == "Aworkit") { result = hwnd; return false; } return true;
    }, IntPtr.Zero); return result;
  }
}
'@
[void][LayoutWindow]::SetThreadDpiAwarenessContext([IntPtr](-4))
$windowHandle = [LayoutWindow]::Find($TargetProcessId)
if ($windowHandle -eq [IntPtr]::Zero) { throw 'Isolated Aworkit window not found' }
switch ($Action) {
  'place' { if (-not [LayoutWindow]::MoveWindow($windowHandle, $X, $Y, $Width, $Height, $true)) { throw 'MoveWindow failed' } }
  'close' { [void][LayoutWindow]::PostMessage($windowHandle, 0x10, [IntPtr]::Zero, [IntPtr]::Zero); return }
  'quit' {
    $fileMenu = [LayoutWindow]::GetSubMenu([LayoutWindow]::GetMenu($windowHandle), 0)
    $itemIndex = [LayoutWindow]::GetMenuItemCount($fileMenu) - 1
    $itemText = New-Object System.Text.StringBuilder 256
    [void][LayoutWindow]::GetMenuString($fileMenu, $itemIndex, $itemText, 256, 0x400)
    if ($itemText.ToString() -notlike 'Quit*') { throw 'Expected File > Quit' }
    $itemId = [LayoutWindow]::GetMenuItemID($fileMenu, $itemIndex)
    [void][LayoutWindow]::PostMessage($windowHandle, 0x111, [IntPtr]$itemId, [IntPtr]::Zero); return
  }
  'minimize' { [void][LayoutWindow]::ShowWindow($windowHandle, 6) }
  'maximize' { [void][LayoutWindow]::ShowWindow($windowHandle, 3) }
  'restore' { [void][LayoutWindow]::ShowWindow($windowHandle, 9) }
}
$outer = New-Object LayoutWindow+Rect
$inner = New-Object LayoutWindow+Rect
[void][LayoutWindow]::GetWindowRect($windowHandle, [ref]$outer)
[void][LayoutWindow]::GetClientRect($windowHandle, [ref]$inner)
@{ x=$outer.Left; y=$outer.Top; width=$outer.Right-$outer.Left; height=$outer.Bottom-$outer.Top; innerWidth=$inner.Right; innerHeight=$inner.Bottom; maximized=[LayoutWindow]::IsZoomed($windowHandle); minimized=[LayoutWindow]::IsIconic($windowHandle) } | ConvertTo-Json -Compress

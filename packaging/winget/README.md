# PwDirEnv winget 打包（占位）

本目录保存 winget manifest 占位与发布说明。**向 winget 官方仓库正式发布不在本仓库
范围内**：需要向 [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs)
提交 PR，通过 manifest validation / 智能校验后合并。此处仅固化两件事：

1. **winget 友好的安装布局**（由 `packaging/install.ps1` 产出）：

   ```text
   <InstallRoot>\
   ├─ bin\pwdir.exe                      # portable 命令（可配 PATH / PortableCommandAlias: pwdir）
   └─ modules\PwDirEnv\
      ├─ PwDirEnv.psd1                   # PowerShell module manifest
      └─ PwDirEnv.psm1
   ```

   该布局同时满足 `pwdir doctor` 的 module 探测（`current_exe` 相对
   `modules\PwDirEnv\PwDirEnv.psd1`）与 `pwdir hook pwsh` 的片段生成。

2. **manifest 骨架**（`PwDirEnv.installer.yaml`）：`InstallerType: zip` +
   `NestedInstallerType: portable`，zip 根目录即上面的 `<InstallRoot>` 布局。
   正式发布前需补齐真实的 InstallerUrl、InstallerSha256 与包元数据
   （Publisher/PackageUrl/LicenseUrl 等），并按 winget 当前 manifest schema
   （1.6+，singleton 或多文件 split）核对字段。

## 手动安装（当前推荐）

```powershell
pwsh -NoProfile -File packaging\install.ps1           # 构建 + 安装到 %LOCALAPPDATA%\Programs\PwDirEnv
pwsh -NoProfile -File packaging\install.ps1 -SkipBuild # 复用已有 target\release 产物
```

脚本不会自动修改系统/用户 PATH；按脚本输出的后续步骤操作即可。

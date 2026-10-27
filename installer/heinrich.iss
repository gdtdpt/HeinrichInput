; 亨利五笔（HeinrichInput）安装包脚本
;
; 编译：ISCC.exe installer\heinrich.iss
;      或：tools\build.ps1 -Installer（自动找 ISCC，找不到就跳过）
; 前提：..\dist 已经由 build.ps1 产出（含 data\ 子目录——运行期靠「exe 同级的
;       data\wubi86 / data\pinyin」定位词库，见 heinrich-server/src/main.rs）。
;
; 设计口径（PROJECT.md D93）：
;   ① 装到 {autopf}\HeinrichInput，管理员权限——与 HKLM 注册同一量级，D19 已定
;      注册必须提权，这里绕不开。
;   ② 卸载**默认保留**用户数据（%APPDATA%\Heinrich / %LOCALAPPDATA%\Heinrich），
;      要删得显式勾选或传 /PURGE——词库、emoji 绑定、学习出来的词频都是用户资产，
;      卸载不等于放弃它们。
;   ③ 删用户数据那一步以**原始用户**身份跑（runasoriginaluser）：提权进程的
;      %APPDATA% 可能是管理员账户的 profile，标准用户输凭据安装时会删错地方。
;   ④ 版本号与 Cargo workspace 同源（build.ps1 传 /DAppVersion）。

#ifndef AppVersion
  #define AppVersion "1.0.0"
#endif

#define AppName "亨利五笔"
#define AppNameEn "HeinrichInput"
#define AppPublisher "Heinrich"
#define SettingsExe "heinrich-settings.exe"
#define SetupExe "heinrich-setup.exe"

[Setup]
; AppId 一旦发布就不能改：Inno 靠它识别「同一个程序的升级/卸载」。
AppId={{244F2EDA-B5E2-4718-AF0E-CE784E66E244}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
VersionInfoVersion={#AppVersion}
VersionInfoDescription={#AppNameEn} 安装程序
DefaultDirName={autopf}\{#AppNameEn}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes
DisableWelcomePage=no
OutputDir=..\dist
OutputBaseFilename=HeinrichSetup-{#AppVersion}
SetupIconFile=..\dist\heinrich.ico
UninstallDisplayIcon={app}\{#SettingsExe}
UninstallDisplayName={#AppName}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; x64 专用：TSF DLL 是 x64 的，装进 32 位系统只会得到一个装得上、用不了的输入法。
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=admin
; 装/卸时会替换 DLL——宿主程序（记事本、浏览器…）正加载着它就换不进去。
; CloseApplications 会列出占用的程序并让用户关掉，比事后「重启才生效」清楚。
CloseApplications=yes
RestartApplications=no
AllowNoIcons=yes

[Languages]
; 官方安装包不带简体中文，这份来自 Inno 源码树 Files\Languages\Unofficial
; （随仓库走，构建机不需要额外下载）。
Name: "chs"; MessagesFile: "ChineseSimplified.isl"

[Files]
Source: "..\dist\{#SetupExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\{#SettingsExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\heinrich_win.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\heinrich-server.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\heinrich-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\heinrich-gen.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\dist\heinrich.ico"; DestDir: "{app}"; Flags: ignoreversion
; 词库必须整目录带过去：缺一份 data 里的文件，server 起不来、所有候选为空。
Source: "..\dist\data\*"; DestDir: "{app}\data"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{group}\{#AppName} 设置"; Filename: "{app}\{#SettingsExe}"
Name: "{group}\卸载 {#AppName}"; Filename: "{uninstallexe}"

[Run]
; 注册那 7 步还是交给 heinrich-setup：它已经处理了「先杀掉旧 server」、
; RegisterProfile / RegisterCategory、以及装完的四项自检。Inno 只负责放文件。
; runhidden + waituntilterminated：进度条上只显示一句状态，不要弹黑框。
Filename: "{app}\{#SetupExe}"; Parameters: "install"; StatusMsg: "正在注册输入法…"; Flags: runhidden waituntilterminated
Filename: "{app}\{#SettingsExe}"; Description: "打开设置窗口"; Flags: postinstall nowait skipifsilent unchecked

[UninstallRun]
; 必须先反注册再删文件：注册表里留着指向已删 DLL 的 CLSID，语言列表里就会
; 留一个「点了报错」的条目。
Filename: "{app}\{#SetupExe}"; Parameters: "uninstall"; Flags: runhidden waituntilterminated; RunOnceId: "UnregisterIme"

[UninstallDelete]
; Inno 只删自己记录过的文件；server 运行期写的东西（若有）留在这里就成垃圾。
Type: filesandordirs; Name: "{app}"

[Code]
{ 命令行里有没有 /PURGE（不区分大小写）。静默卸载只能靠它决定删不删数据。 }
function HasParam(const P: String): Boolean;
var
  I: Integer;
begin
  Result := False;
  for I := 1 to ParamCount do
    if CompareText(ParamStr(I), P) = 0 then
    begin
      Result := True;
      Exit;
    end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Purge: Boolean;
  ErrCode: Integer;
begin
  { usUninstall 阶段：文件还没删，heinrich-setup.exe 还在。
    usPostUninstall 就晚了——那时程序目录已经空了。 }
  if CurUninstallStep <> usUninstall then
    Exit;

  Purge := HasParam('/PURGE');

  { 静默卸载（/VERYSILENT）不弹窗，默认保留；非静默时问一句，默认「否」。 }
  if (not Purge) and (not UninstallSilent) then
    Purge := SuppressibleMsgBox(
      '是否同时删除用户数据？' + #13#10#13#10 +
      '包括：自定义词组（words.tsv）、emoji 绑定（emoji.user.toml）、' +
      '配置（config.toml）、学习库（learn.db）与日志。' + #13#10#13#10 +
      '删除后无法恢复；选择「否」会保留它们，重装后可以接着用。',
      mbConfirmation, MB_YESNO, IDNO) = IDYES;

  if not Purge then
    Exit;

  { runasoriginaluser：这段代码跑在卸载器进程里（提权态），但 purge 要删的是
    登录用户的 %APPDATA%，提权进程展开的路径可能是管理员账户的。 }
  if not ExecAsOriginalUser(ExpandConstant('{app}\{#SetupExe}'), 'purge', '',
                            SW_HIDE, ewWaitUntilTerminated, ErrCode) then
    MsgBox('删除用户数据失败（无法启动 heinrich-setup purge）。' + #13#10 +
           '可以稍后手动执行：heinrich-setup.exe purge', mbError, MB_OK);
end;

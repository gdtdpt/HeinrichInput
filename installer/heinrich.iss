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
;   ⑤ 升级不再要求关掉宿主程序（D103）：Windows 只是禁止**删除/原地覆盖**被映射的
;      文件，**改名是允许的**（实测：映射中 rename 成功、delete 被拒）。所以安装前
;      把 heinrich_win.dll 等改名为 .old，新文件照常落到原路径；老进程跟着 .old 继续跑。

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
; 装/卸时会替换 DLL——宿主程序（浏览器、微信、编辑器…）正映射着它就换不进去。
; D103：**不再**用 CloseApplications 逼用户关程序。Restart Manager 只认「文件被
; 映射着」，不认「输入法是否还在用」，于是「用得越顺的程序越装不上」，还会陷入
; 「关掉 → 为了看提示又打开 → 又被判定占用」的死循环。改为在 ssInstall 阶段把
; 被映射的文件改名成 .old（见 [Code]），安装照常完成。
CloseApplications=no
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

{ ================= D103：不关宿主程序也能升级 =================

  Windows 对「已被映射（mapped）的文件」只禁止两件事：删除、原地覆盖。
  **改名是允许的**（安装包里实测：映射中的 DLL 改名成功、删除被拒绝）。
  所以升级时先把 heinrich_win.dll 之类改名成 .old，新文件就能写回原路径；
  老进程的映射跟着 .old 继续跑，不崩、不断输入。

  为什么不继续用 CloseApplications：Restart Manager 只认「文件被谁映射着」，
  不认「这个程序还在不在用输入法」。用户即便已经切走输入法、甚至已卸载注册，
  只要有进程映射着 DLL，它照样拦——而关掉它再看提示又会把进程重新拉起来。 }

{ 可能被宿主程序映射着的文件（DLL 首当其冲；exe 被直接运行时也会被占用）。 }
function LockableName(Index: Integer): String;
begin
  case Index of
    0: Result := 'heinrich_win.dll';
    1: Result := 'heinrich-server.exe';
    2: Result := 'heinrich-settings.exe';
    3: Result := 'heinrich-setup.exe';
    4: Result := 'heinrich-cli.exe';
    5: Result := 'heinrich-gen.exe';
  else
    Result := '';
  end;
end;

{ 把安装目录下的 Name 改名成 Name.old / Name.old1 / …。返回是否改名成功。 }
function UnlockOne(const Dir, Name: String): Boolean;
var
  Src, Dst: String;
  I: Integer;
begin
  Result := False;
  Src := AddBackslash(Dir) + Name;
  if not FileExists(Src) then
    Exit;

  for I := 0 to 9 do
  begin
    if I = 0 then
      Dst := Src + '.old'
    else
      Dst := Src + '.old' + IntToStr(I);

    { 上次升级留下的同名 .old：还删得掉就复用这个后缀，删不掉（仍被映射）换下一个。 }
    if (not FileExists(Dst)) or DeleteFile(Dst) then
    begin
      if RenameFile(Src, Dst) then
      begin
        Result := True;
        Exit;
      end;
    end;
  end;
end;

procedure UnlockInstalledFiles;
var
  Dir: String;
  I: Integer;
begin
  Dir := ExpandConstant('{app}');
  for I := 0 to 5 do
    UnlockOne(Dir, LockableName(I));
end;

{ 装完清掉 .old；清不掉说明还有老进程映射着，留给下次升级或卸载。 }
procedure CleanupOldFiles;
var
  Dir, Src, Dst: String;
  I, J: Integer;
begin
  Dir := ExpandConstant('{app}');
  for I := 0 to 5 do
  begin
    Src := AddBackslash(Dir) + LockableName(I);
    for J := 0 to 9 do
    begin
      if J = 0 then
        Dst := Src + '.old'
      else
        Dst := Src + '.old' + IntToStr(J);
      if FileExists(Dst) then
        DeleteFile(Dst);
    end;
  end;
end;

{ 列出仍映射着旧 DLL 的进程名（去重、用「、」连接）。空串 = 没有。 }
function OldHostProcesses: String;
var
  Tmp, Cmd, Line, Name, Seen: String;
  Code, I, P: Integer;
  Lines: TArrayOfString;
begin
  Result := '';
  Tmp := ExpandConstant('{tmp}\heinrich-oldhosts.txt');
  DeleteFile(Tmp);
  Cmd := '/c ' + ExpandConstant('{sys}\tasklist.exe') +
         ' /m heinrich_win.dll /fo csv /nh > "' + Tmp + '" 2>nul';
  if not Exec(ExpandConstant('{cmd}'), Cmd, '', SW_HIDE, ewWaitUntilTerminated, Code) then
    Exit;
  if not LoadStringsFromFile(Tmp, Lines) then
    Exit;
  DeleteFile(Tmp);

  Seen := '|';
  for I := 0 to GetArrayLength(Lines) - 1 do
  begin
    Line := Trim(Lines[I]);
    P := Pos(',', Line);
    if P > 1 then
    begin
      Name := RemoveQuotes(Copy(Line, 1, P - 1));
      if (Name <> '') and (Pos('|' + LowerCase(Name) + '|', Seen) = 0) then
      begin
        Seen := Seen + LowerCase(Name) + '|';
        if Result <> '' then
          Result := Result + '、';
        Result := Result + Name;
      end;
    end;
  end;
end;

{ 卸载时宿主还映射着 DLL 的话，卸载器会把删除登记成「下次重启执行」。
  若用户没重启就重装，那条登记到重启时会把**新**装的文件删掉——查一下，
  命中就提醒（reg query 把 MULTI_SZ 按行输出，一行一项）。 }
function PendingDeleteWarning: String;
var
  Tmp, Cmd, Line: String;
  Code, I: Integer;
  Lines: TArrayOfString;
begin
  Result := '';
  Tmp := ExpandConstant('{tmp}\heinrich-pending.txt');
  DeleteFile(Tmp);
  Cmd := '/c ' + ExpandConstant('{sys}\reg.exe') +
         ' query "HKLM\SYSTEM\CurrentControlSet\Control\Session Manager"' +
         ' /v PendingFileRenameOperations > "' + Tmp + '" 2>nul';
  if not Exec(ExpandConstant('{cmd}'), Cmd, '', SW_HIDE, ewWaitUntilTerminated, Code) then
    Exit;
  if not LoadStringsFromFile(Tmp, Lines) then
    Exit;
  DeleteFile(Tmp);

  for I := 0 to GetArrayLength(Lines) - 1 do
  begin
    Line := Lines[I];
    if Pos('{#AppNameEn}', Line) > 0 then
    begin
      Result := #13#10#13#10 +
        '注意：系统里还留着上次卸载登记的「重启后删除」，' +
        '下次重启可能把新版文件删掉。' + #13#10 +
        '若重启后输入法失效，重新运行一次安装包即可复原。';
      Exit;
    end;
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Msg, Hosts: String;
begin
  if CurStep = ssInstall then
    UnlockInstalledFiles
  else if CurStep = ssPostInstall then
    CleanupOldFiles
  else if CurStep = ssDone then
  begin
    Hosts := OldHostProcesses;
    Msg := PendingDeleteWarning;
    if (Hosts = '') and (Msg = '') then
      Exit;

    if Hosts <> '' then
      Msg := #13#10#13#10 + '这些程序还在用旧版输入法：' + Hosts + #13#10 +
             '重启它们后才会用上新版；新打开的程序已经是新版，不用重启电脑。' + Msg;

    SuppressibleMsgBox('安装完成。' + Msg, mbInformation, MB_OK, IDOK);
  end;
end;

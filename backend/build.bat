@echo off
setlocal

REM Output directory (relative to project root)
set BUILD_DIR=..\builds

REM Create build directory if it doesn't exist
if not exist "%BUILD_DIR%" (
    mkdir "%BUILD_DIR%"
)

echo Building Windows binary...
set GOOS=windows
set GOARCH=amd64
go build -o "%BUILD_DIR%\gaggle-backend-windows-amd64.exe"
if errorlevel 1 goto build_error

echo Building Linux binary...
set GOOS=linux
set GOARCH=amd64
go build -o "%BUILD_DIR%\gaggle-backend-linux-amd64"
if errorlevel 1 goto build_error

echo.
echo Build completed successfully!
goto end

:build_error
echo.
echo Build failed!
exit /b 1

:end
endlocal

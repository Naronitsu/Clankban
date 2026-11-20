const { app, BrowserWindow, dialog } = require('electron');
const path = require('path');
const { startServer, closeServer } = require('./server');

let mainWindow;
let apiServer;

async function createWindow() {
  apiServer = await startServer({ port: Number(process.env.CLANKBAN_PORT) || 4187, dataDir: app.getPath('userData') });
  mainWindow = new BrowserWindow({
    width: 1440,
    height: 960,
    minWidth: 980,
    minHeight: 680,
    backgroundColor: '#f7f8fc',
    webPreferences: { preload: path.join(__dirname, 'preload.js'), contextIsolation: true, nodeIntegration: false }
  });
  mainWindow.loadFile(path.join(__dirname, 'ui', 'index.html'), { query: { api: apiServer.url } });
  mainWindow.on('closed', () => { mainWindow = null; });
}

app.whenReady().then(createWindow).catch(error => dialog.showErrorBox('Clankban could not start', error.stack || String(error)));
app.on('window-all-closed', async () => { await closeServer(apiServer); if (process.platform !== 'darwin') app.quit(); });
app.on('activate', () => { if (!mainWindow) createWindow(); });

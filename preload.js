const { contextBridge } = require('electron');
const apiUrl = new URLSearchParams(window.location.search).get('api');
contextBridge.exposeInMainWorld('clankban', { apiUrl, desktop: true });

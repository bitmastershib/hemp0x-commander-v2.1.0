const http = require('http');
const fs = require('fs');
const os = require('os');
const path = require('path');

function getAuth() {
    let confPath = path.join(os.homedir(), 'AppData', 'Roaming', 'Hemp0x', 'hemp.conf');
    if (fs.existsSync(confPath)) {
        let lines = fs.readFileSync(confPath, 'utf8').split('\n');
        let user = '', pass = '';
        for (let line of lines) {
            if (line.startsWith('rpcuser=')) user = line.split('=')[1].trim();
            if (line.startsWith('rpcpassword=')) pass = line.split('=')[1].trim();
        }
        if (user && pass) return Buffer.from(user + ':' + pass).toString('base64');
    }
    return '';
}

function rpc(method, params) {
    return new Promise((resolve, reject) => {
        const req = http.request({
            host: '127.0.0.1',
            port: 42068,
            method: 'POST',
            headers: {
                'Content-Type': 'application/json',
                'Authorization': 'Basic ' + getAuth()
            }
        }, res => {
            let data = '';
            res.on('data', d => data += d);
            res.on('end', () => resolve(JSON.parse(data)));
        });
        req.on('error', reject);
        req.write(JSON.stringify({ jsonrpc: '1.0', id: 'test', method, params }));
        req.end();
    });
}

async function run() {
    try {
        console.log("Creating dummy tx...");
        // This is a dummy tx. We just need to see if signrawtransaction gives a signature!
        // We will test signrawtransaction with a random txid, but valid scriptPubKey and redeemScript.
        // First get a new address
        let addrRes = await rpc('getnewaddress', []);
        let addr = addrRes.result;
        let privRes = await rpc('dumpprivkey', [addr]);
        let privkey = privRes.result;
        
        let valRes = await rpc('validateaddress', [addr]);
        let pubkey = valRes.result.pubkey;
        let pubkeyHash = valRes.result.scriptPubKey.slice(6, 46); // extract hash160

        // Build a dummy OP_IF script matching our HTLC:
        // OP_IF OP_SHA256 <hash> OP_EQUALVERIFY OP_DUP OP_HASH160 <hash> OP_EQUALVERIFY OP_CHECKSIG OP_ELSE <locktime> OP_CHECKLOCKTIMEVERIFY OP_DROP OP_DUP OP_HASH160 <hash> OP_EQUALVERIFY OP_CHECKSIG OP_ENDIF
        // We don't need to fully build it, we just want to see if signrawtransaction works on it!
        // Actually, let's just make a very simple P2SH script: OP_IF OP_DUP OP_HASH160 <hash> OP_EQUALVERIFY OP_CHECKSIG OP_ELSE OP_DUP OP_HASH160 <hash> OP_EQUALVERIFY OP_CHECKSIG OP_ENDIF
        let redeemScript = "6376a914" + pubkeyHash + "88ac6776a914" + pubkeyHash + "88ac68";
        
        let p2shRes = await rpc('decodescript', [redeemScript]);
        let p2shAddr = p2shRes.result.p2sh;
        let scriptPubKey = "a914" + p2shRes.result.p2sh + "87"; // This is wrong, let's use the actual scriptPubKey. 
        // p2sh address is base58. P2SH scriptPubKey is OP_HASH160 <20bytes> OP_EQUAL
        
        // Wait, an easier way: send to this P2SH address, then try to spend it!
        console.log("Sending 0.001 HEMP to P2SH:", p2shAddr);
        let txidRes = await rpc('sendtoaddress', [p2shAddr, 0.001]);
        if (txidRes.error) {
            console.log("Send failed:", txidRes.error);
            return;
        }
        let txid = txidRes.result;
        
        // Find vout
        let getTxRes = await rpc('getrawtransaction', [txid, true]);
        let voutObj = getTxRes.result.vout.find(v => v.scriptPubKey.addresses && v.scriptPubKey.addresses[0] === p2shAddr);
        let vout = voutObj.n;
        let spk = voutObj.scriptPubKey.hex;
        
        // Create raw tx spending it
        let createRes = await rpc('createrawtransaction', [[{txid, vout}], {[addr]: 0.0005}]);
        let rawTx = createRes.result;
        
        // Sign it!
        let signRes = await rpc('signrawtransaction', [rawTx, [{txid, vout, scriptPubKey: spk, redeemScript, amount: 0.001}]]);
        console.log("Sign result:", JSON.stringify(signRes, null, 2));
        
        // Decode the partial hex to see scriptSig
        if (signRes.result && signRes.result.hex) {
            let decodeRes = await rpc('decoderawtransaction', [signRes.result.hex]);
            console.log("ScriptSig:", decodeRes.result.vin[0].scriptSig);
        }
        
    } catch (e) {
        console.error(e);
    }
}

run();

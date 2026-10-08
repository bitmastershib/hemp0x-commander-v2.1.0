<script>
  import { onMount } from "svelte";
  import { get } from "svelte/store";
  import { core } from "@tauri-apps/api";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { listen } from "@tauri-apps/api/event";
  import {
    nodeStatus,
    walletInfo as walletStore,
    networkInfo,
    systemStatus as systemStore,
    daemonRuntime,
    coreBusyUntil,
  } from "./stores.js";
  import { addRuntimeNotification } from "./lib/stores/notifications.js";

  import logoNew from "./assets/logonew.png";
  import eyeOpen from "./assets/eye-open.png";
  import eyeClosed from "./assets/eye-closed.png";
  import ViewSend from "./lib/ViewSend.svelte";
  import ViewReceive from "./lib/ViewReceive.svelte";
  import ViewAssets from "./lib/ViewAssets.svelte";
  import ViewTools from "./lib/ViewTools.svelte";
  import NotificationCenter from "./lib/ui/NotificationCenter.svelte";
  import VaultUnlockModal from "./lib/ui/VaultUnlockModal.svelte";

  // Ravencoin Multi-Chain Components
  import ViewRavencoin from "./lib/ravencoin/ViewRavencoin.svelte";
  import AtomicSwap from "./lib/crosschain/AtomicSwap.svelte";
  import {
    loadWalletPinStatus,
    defaultUnlockMode,
    pinRequiresPassphrase,
    unlockWalletWithPin,
    unlockRuntimeWalletWithPassphrase,
    forgotWalletPin,
    isValidPin,
    cleanWalletPin,
  } from "./lib/walletPinUnlock.js";
  import CommanderLoader from "./lib/ui/CommanderLoader.svelte";
  import CopyIcon from "./lib/ui/CopyIcon.svelte";
  import { stratumStatus } from "./lib/stores/stratum.js";
  import { cidViewerTarget, ipfsHubSection } from "./lib/stores/contentLibrary.js";
  import { systemHubSection } from "./lib/stores/systemHub.js";
  import { vaultStatus } from "./stores.js";
  import { APP_VERSION } from "./lib/constants.js";
  import { initNostr } from "./lib/stores/nostrStore.js";
  import FastSyncModal from "./lib/ui/FastSyncModal.svelte";

  let showHempFastSyncModal = false;
  let hempSnapshotInstalled = typeof localStorage !== "undefined" && localStorage.getItem("hemp0x_snapshot_installed") === "true";

  let coreBusyUntilMs = 0;
  const unsubscribeCoreBusy = coreBusyUntil.subscribe((value) => {
    coreBusyUntilMs = Number(value || 0);
  });
  const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

  /**
   * @typedef {{ name: string, address: string, category: string, amount: string|number, time: number|string, date?: string, type?: string, txid?: string, asset?: string, conf?: number, confirmations?: number, direction?: string }} RecentTx
   * @typedef {{ state: string, blocks: string|number, headers: string|number, peers: string|number, diff: string|number, synced: boolean }} NodeInfo
   * @typedef {{ balance: string, pending: string, staked: string, status: string }} WalletInfo
   * @typedef {{ auth_mode?: string, warning?: string }} RpcAuthStatus
   * @typedef {{
   *   identity?: {
   *     rpc_authenticated?: boolean,
   *     base_version?: string|null,
   *     subversion?: string|null,
   *     protocol_version?: number|null,
   *     numeric_version?: number|null,
   *     is_required_core_next?: boolean,
   *     commit_match?: boolean,
   *     commit_available?: boolean,
   *     status?: string,
   *     capabilities?: Record<string, any>|null
   *   }|null,
   *   processIdentity?: any,
   * } & Record<string, any>} ConflictRuntimeStatus
   * @typedef {{ state: "RUNNING" | "OFFLINE" | string }} StratumState
   */

  // --- STATE ---
  let activeTab = "DASHBOARD"; // DASHBOARD, SEND, RECEIVE, ASSETS, TOOLS, ABOUT
  /** @type {string | null} */
  let lastCidTarget = null;
  $: if ($cidViewerTarget && $cidViewerTarget !== lastCidTarget) {
      lastCidTarget = $cidViewerTarget;
      activeTab = "TOOLS";
  }

  // --- DATA (Populated from daemon) ---
  /** @type {NodeInfo} */
  let nodeInfo = {
    state: "--",
    blocks: "--",
    headers: "--",
    peers: "--",
    diff: "--",
    synced: false, // Conservative default until sync status is confirmed
  };

  /** @type {WalletInfo} */
  let walletInfo = {
    balance: "--",
    pending: "--",
    staked: "--",
    status: "--",
  };

  /** @type {RecentTx[]} */
  let recentTx = [];
  let lastError = "";
  let tauriReady = false;
  let sessionStamp = "";
  let uiScale = 1;
  let isRefreshing = false; // Prevent overlapping refresh calls
  let networkMode = "mainnet"; // mainnet, testnet, regtest
  let rpcFailCount = 0;
  let rpcBackoffUntil = 0; // timestamp ms until which RPC is skipped
  /** @type {{ node: NodeInfo, wallet: WalletInfo, tx: RecentTx[] } | null} */
  let lastKnownDashboard = null; // preserve last good data across transient errors
  let dashboardFailCount = 0;
  let repairStatus = null;
  let repairStatusRefreshing = false;

  // Vault status
  let vaultLocked = true;
  let vaultExists = false;
  let showVaultUnlockModal = false;
  let vaultUnlockPassphrase = "";
  let vaultUnlocking = false;
  let vaultUnlockError = "";

  // Runtime notification state tracking (prevents duplicate notifications)
  let coreNextReadyNotified = false;
  let coreNextMismatchNotified = false;
  let lastDaemonStartSuccessAt = 0;

  // Daemon operation state (prevents spamming start/stop and shows live status)
  let daemonOperation = "idle"; // "idle" | "starting" | "stopping"
  let daemonStatusMessage = "";
  let daemonPollProgress = "";
  let daemonStatusClearTimer = null;

  $: daemonState = $daemonRuntime;
  $: stratumState = $stratumStatus;
  $: stratumRunning =
    stratumState.state === "RUNNING" || stratumState.state === "STARTING";
  $: bundledDaemon = daemonState.daemon || {};
  $: coreNextKnown =
    Boolean(bundledDaemon.exists) ||
    Boolean(bundledDaemon.base_version) ||
    Boolean(bundledDaemon.commit_hash);
  $: coreNextOk = bundledDaemon.exact_core_next_match === true;
  $: coreNextVersion = bundledDaemon.base_version
    ? `v${bundledDaemon.base_version}${bundledDaemon.commit_hash ? `-${bundledDaemon.commit_hash}` : ""}`
    : "--";
  $: repairActive = repairStatus?.active === true;
  $: repairModeLabel =
    repairStatus?.mode === "reindex-chainstate" ? "CHAINSTATE REINDEX" : "REINDEXING";
  $: repairDisplayPhase =
    daemonOperation === "stopping"
      ? "Stopping daemon; repair will resume on next start"
      : daemonOperation === "starting" && repairActive
        ? "Starting daemon with repair flag"
        : repairStatus?.phase || repairModeLabel;
  $: repairProgress =
    typeof repairStatus?.verification_progress === "number"
      ? Math.max(0, Math.min(100, repairStatus.verification_progress * 100))
      : null;
  $: syncLabel = daemonOperation === "stopping"
    ? "STOPPING"
    : repairActive
    ? repairModeLabel
    : nodeInfo.synced
      ? "SYNCED"
      : nodeInfo.state === "RUNNING"
        ? "SYNCING"
        : "--";
  $: conflictCapabilities = conflictRuntimeStatus?.identity?.capabilities;
  $: conflictCapabilityLabels = conflictCapabilities
    ? [
        conflictCapabilities.wallet_migration ? "migration" : null,
        conflictCapabilities.messaging ? "messaging" : null,
        conflictCapabilities.restricted_assets ? "restricted assets" : null,
        conflictCapabilities.qualifiers ? "qualifiers" : null,
        conflictCapabilities.rewards ? "rewards" : null,
        conflictCapabilities.snapshots ? "snapshots" : null,
        conflictCapabilities.has_view_channel_messages ? "channel messages" : null,
        conflictCapabilities.has_message_txid_lookup ? "txid lookup" : null,
      ].filter(Boolean)
    : [];

  // --- PERSISTENT CONSOLE STATE ---
  let globalConsoleOutput = "";
  /** @type {string[]} */
  let globalConsoleHistory = [];

  // --- WELCOME POPUP ---
  let showWelcome = false;
  let showWelcomeOnStartup = true; // Default ON
  let disclaimerScrolled = false;
  /** @type {HTMLElement | null} */
  let disclaimerBodyEl = null;

  // --- HIDE BALANCE ---
  // Default to true (hidden) if not set, or restore from storage
  let hideBalance = localStorage.getItem("hemp0x_hideBalance") !== "false";

  function toggleHideBalance() {
    hideBalance = !hideBalance;
    localStorage.setItem("hemp0x_hideBalance", hideBalance.toString());
  }

  // --- HIDE ACTIVITY ---
  // Default to true (hidden) if not set, or restore from storage
  let hideActivity = localStorage.getItem("hemp0x_hideActivity") !== "false";

  function toggleHideActivity() {
    hideActivity = !hideActivity;
    localStorage.setItem("hemp0x_hideActivity", hideActivity.toString());
  }

  // --- DAEMON LIFECYCLE ---
  let showDaemonConflict = false;
  /** @type {ConflictRuntimeStatus | null} */
  let conflictRuntimeStatus = null;
  let conflictResolved = false;
  let closeCleanupInProgress = false;
  let closeCleanupComplete = false;
  let showClosePrompt = false;
  let closePromptStatus = "";
  /** @type {RpcAuthStatus | null} */
  let rpcAuthStatus = null;
  let showRpcAuthHelp = false;
  let appSettings = {
    auto_start_daemon_on_launch: false,
    keep_daemon_running_on_close: false,
    allow_non_bundled_core_next: false,
  };

  /**
   * @param {string} choice
   */
  async function resolveDaemonConflict(choice) {
    showDaemonConflict = false;
    if (choice === "continue") {
      await core.invoke("release_daemon_ownership");
      addRuntimeNotification("Using existing daemon", "", "info");
      // An existing daemon may not have the active vault wallet loaded.
      await verifyActiveVaultWalletLoaded();
    } else if (choice === "stop_and_use_bundled") {
      try {
        await core.invoke("stop_node");
        addRuntimeNotification("External daemon stopped", "", "info");
        await core.invoke("start_node");
        addRuntimeNotification("Bundled daemon started", "", "info");
        await core.invoke("take_daemon_ownership");
        daemonRuntime.update((d) => ({ ...d, commanderOwns: true }));
        const readiness = await core.invoke("wait_for_daemon_ready", { timeoutMs: 25000 });
        daemonRuntime.update((d) => ({ ...d, readiness }));
        if (!readiness.ready) {
          lastError = "Daemon did not become ready after start: " + (readiness.rpc_error || "timeout");
          addRuntimeNotification("Daemon readiness failed", lastError, "error");
        } else {
          await refreshRpcAuthStatus();
          lastDaemonStartSuccessAt = Date.now();
          addRuntimeNotification("Bundled daemon ready", "", "success");
          // Verify the active vault wallet is loaded after switching daemons.
          await verifyActiveVaultWalletLoaded();
        }
      } catch (e) {
        lastError = String(e || "Failed to switch daemon");
        addRuntimeNotification("Daemon switch failed", lastError, "error");
      }
    } else {
      addRuntimeNotification("Dismissed", "Daemon dialog dismissed.", "info");
    }
    conflictResolved = true;
    daemonRuntime.update((d) => ({ ...d, conflictResolved: true }));
    setTimeout(refreshDashboard, 1500);
  }

  async function loadAppSettings() {
    try {
      appSettings = await core.invoke("load_app_settings");
      daemonRuntime.update((d) => ({
        ...d,
        settings: {
          auto_start_daemon_on_launch: appSettings.auto_start_daemon_on_launch,
          keep_daemon_running_on_close: appSettings.keep_daemon_running_on_close,
          allow_non_bundled_core_next: appSettings.allow_non_bundled_core_next,
        },
      }));
    } catch {
      // use defaults
    }
    await refreshRpcAuthStatus();
  }

  async function refreshRpcAuthStatus() {
    try {
      rpcAuthStatus = await core.invoke("get_rpc_auth_status");
    } catch {
      // auth status probe is best-effort
    }
  }

  function openRpcAuthHelp() {
    if (rpcAuthStatus?.auth_mode === "cookie") return;
    showRpcAuthHelp = true;
  }

  function openRpcAuthConfig() {
    showRpcAuthHelp = false;
    systemHubSection.set("config");
    activeTab = "TOOLS";
    window.dispatchEvent(new CustomEvent("commander-open-tools-system", { detail: { section: "config" } }));
  }

  function timeoutResult(ms, message) {
    return new Promise((resolve) => {
      setTimeout(() => resolve({ timedOut: true, error: message }), ms);
    });
  }

  async function invokeWithCloseWatchdog(command, args, timeoutMs, timeoutMessage) {
    return Promise.race([
      core.invoke(command, args).then((value) => ({ value })).catch((error) => ({ error })),
      timeoutResult(timeoutMs, timeoutMessage),
    ]);
  }

  async function exitCommanderProcess() {
    closeCleanupComplete = true;
    showClosePrompt = false;
    closeCleanupInProgress = true;
    try { await invokeWithCloseWatchdog("rvn_stop_node_and_wait", { timeout_ms: 30000 }, 32000, ""); } catch(e) {}
    try {
      await core.invoke("exit_commander");
    } catch {
      try {
        await getCurrentWindow().close();
      } catch {
        // The backend exit command is authoritative; this is only a fallback.
      }
    }
  }

  async function closeStopDaemon() {
    closePromptStatus = "Stopping daemon...";
    closeCleanupInProgress = true;
    const closeWaitMs = 30000;
    try {
      const stop = await invokeWithCloseWatchdog(
        "stop_node_and_wait",
        { timeoutMs: 20000 },
        closeWaitMs,
        "Commander waited for Core to stop, but Core did not answer in time."
      );
      if (stop.error) {
        addRuntimeNotification("Daemon stop on close timed out", String(stop.error || "").substring(0, 240), "warning");
      }
    } catch (err) {
      addRuntimeNotification("Daemon stop on close timed out", String(err || "").substring(0, 240), "warning");
    }
    closePromptStatus = "Closing Commander...";
    try {
      await invokeWithCloseWatchdog(
        "release_daemon_ownership",
        {},
        3000,
        "Commander could not release daemon ownership before closing."
      );
    } catch {
      // best-effort cleanup
    }
    await exitCommanderProcess();
  }

  async function closeLeaveDaemon() {
    closePromptStatus = "Closing Commander...";
    closeCleanupInProgress = true;
    try {
      await invokeWithCloseWatchdog(
        "release_daemon_ownership",
        {},
        3000,
        "Commander could not release daemon ownership before closing."
      );
    } catch {
      // best-effort cleanup
    }
    await exitCommanderProcess();
  }

  function closeCancel() {
    showClosePrompt = false;
    closeCleanupInProgress = false;
    closePromptStatus = "";
  }

  function closeWelcome() {
    localStorage.setItem(
      "hemp0x_showWelcome",
      showWelcomeOnStartup ? "1" : "0",
    );
    showWelcome = false;
  }

  /** @param {Event} e */
  function handleDisclaimerScroll(e) {
    const el = e.target;
    if (el instanceof HTMLElement) {
      updateDisclaimerScrolled(el);
    }
  }

  function updateDisclaimerScrolled(el = disclaimerBodyEl) {
    if (!el) return;
    const noScrollNeeded = el.scrollHeight <= el.clientHeight + 2;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 30;
    if (noScrollNeeded || atBottom) disclaimerScrolled = true;
  }

  function checkWelcomePopup() {
    const stored = localStorage.getItem("hemp0x_showWelcome");
    showWelcomeOnStartup = stored !== "0";
    if (showWelcomeOnStartup) {
      disclaimerScrolled = false;
      showWelcome = true;
      setTimeout(() => updateDisclaimerScrolled(), 0);
    }
  }

  // System status for traffic light indicators
  // green = all systems go, yellow = syncing/partial, red = offline/error
  $: systemStatus = (() => {
    const nodeRunning = nodeInfo.state === "RUNNING";
    const walletLoaded =
      walletInfo.status !== "--" && walletInfo.status !== "UNLOADED";
    const isSynced = nodeInfo.synced === true; // From blockchain sync check

    if (nodeRunning && walletLoaded && isSynced) {
      return "green"; // All systems go
    } else if (nodeRunning && (!isSynced || !walletLoaded)) {
      return "yellow"; // Syncing or partial connection
    } else {
      return "red"; // Disconnected or error
    }
  })();
  let showWalletPrompt = false;
  let walletPromptMode = "unlock";
  let walletPromptPass = "";
  let walletPromptPassConfirm = "";
  let walletPromptDuration = "60";
  let walletPromptError = "";
  // Runtime Wallet PIN unlock (slice 76b). Applies to the unlock mode only.
  let walletPromptPin = "";
  let walletPromptMode2 = "passphrase"; // "passphrase" | "pin" (PIN entry within the unlock prompt)
  let walletPinStatus = null;
  let walletPromptWorking = false;
  $: walletPinUsable = !!walletPinStatus?.pin_configured && !pinRequiresPassphrase(walletPinStatus);
  // --- DASHBOARD OVERHAUL ---
  let isActivityExpanded = false;

  function toggleActivityExpand() {
    isActivityExpanded = !isActivityExpanded;
  }

  function openHistoryPage() {
    activeTab = "TOOLS";
    window.dispatchEvent(new CustomEvent("commander-open-tools-history"));
  }

  function openExplorer(target) {
    if (!target) return;
    window.dispatchEvent(
      new CustomEvent("commander-open-explorer", {
        detail: { target },
      }),
    );
  }

  async function copyRecentTxid(txid) {
    if (!txid) return;
    try {
      await navigator.clipboard.writeText(txid);
    } catch {
      addRuntimeNotification("Copy failed", "Could not copy the transaction ID.", "warning");
    }
  }

  /**
   * @param {string} tab
   */
  function isActive(tab) {
    return activeTab === tab;
  }
  /**
   * @param {string} tab
   */
  function setTab(tab) {
    activeTab = tab;
  }

  function setOffline(reason = "") {
    if (nodeInfo.state === "OFFLINE" && lastError === reason) return;

    if (nodeInfo.state !== "OFFLINE") {
      addRuntimeNotification("Daemon offline", reason || "Node connection lost.", "warning");
    }

    lastKnownDashboard = null;
    nodeInfo = {
      state: "OFFLINE",
      blocks: "--",
      headers: "--",
      peers: "--",
      diff: "--",
      synced: false,
    };
    walletInfo = {
      balance: "--",
      pending: "--",
      staked: "--",
      status: "--",
    };
    recentTx = [];
    lastError = reason;

    // UPDATE STORES (OFFLINE)
    nodeStatus.set({
      online: false,
      version: "--",
      connections: 0,
      headers: 0,
      blocks: 0,
      verificationProgress: 0,
      error: reason,
    });
    walletStore.set({
      balance: "0.00",
      unconfirmed: 0.0,
      immature: 0.0,
      transactions: [],
      newTxCount: 0,
      status: "--",
    });
  }

  async function refreshDashboard() {
    if (isRefreshing) return;
    isRefreshing = true;
    try {
      if (!tauriReady) {
        setOffline("Tauri backend not available.");
        return;
      }

      const tryRpc = rpcFailCount < 3 && Date.now() > rpcBackoffUntil;
      let data = null;

      if (tryRpc) {
        try {
          data = await core.invoke("rpc_dashboard");
          rpcFailCount = 0;
          rpcBackoffUntil = 0;
        } catch (rpcErr) {
          rpcFailCount++;
          if (rpcFailCount >= 3) {
            rpcBackoffUntil = Date.now() + 120000;
          } else {
            rpcBackoffUntil = Date.now() + 1000;
          }
        }
      }

      if (!data) {
        data = await core.invoke("dashboard_data");
        if (tryRpc) {
          rpcFailCount = Math.min(rpcFailCount, 2);
        }
      }

      lastKnownDashboard = data;
      dashboardFailCount = 0;

      const wasNotRunning = nodeInfo.state !== "RUNNING";
      nodeInfo = data.node;
      walletInfo = data.wallet;
      recentTx = data.tx;
      lastError = "";

      const blocks = parseInt(data.node.blocks) || 0;
      if (!hempSnapshotInstalled && (data.node.synced || blocks > 1000)) {
        hempSnapshotInstalled = true;
        localStorage.setItem("hemp0x_snapshot_installed", "true");
      }

      if (
        wasNotRunning &&
        data.node.state === "RUNNING" &&
        Date.now() - lastDaemonStartSuccessAt > 5000
      ) {
        addRuntimeNotification("Daemon running", "Node is connected and responding.", "success");
      }

      nodeStatus.set({
        online: data.node.state === "RUNNING",
        version: "--",
        connections: parseInt(data.node.peers) || 0,
        headers: parseInt(data.node.headers) || 0,
        blocks: parseInt(data.node.blocks) || 0,
        verificationProgress: 0,
        error: null,
      });

      walletStore.set({
        balance: data.wallet.balance || "0.00",
        unconfirmed: parseFloat(data.wallet.pending) || 0.0,
        immature: parseFloat(data.wallet.staked) || 0.0,
        transactions: data.tx || [],
        newTxCount: 0,
        status: data.wallet.status || "--",
      });

      networkInfo.update((n) => ({
        ...n,
        difficulty: parseFloat(data.node.diff) || 0,
      }));
    } catch (err) {
      if (lastKnownDashboard) {
        dashboardFailCount++;
        nodeInfo = {
          ...lastKnownDashboard.node,
          state: dashboardFailCount >= 3 ? "OFFLINE" : lastKnownDashboard.node.state,
          synced: dashboardFailCount >= 3 ? false : lastKnownDashboard.node.synced,
        };
        walletInfo = lastKnownDashboard.wallet;
        recentTx = lastKnownDashboard.tx;
        lastError = String(err || "RPC error");
        nodeStatus.update((current) => ({
          ...current,
          online: dashboardFailCount < 3 && current.online,
          error: lastError,
        }));
      } else {
        setOffline(String(err || "RPC error"));
      }
    } finally {
      isRefreshing = false;
    }
  }

  async function refreshRepairStatus() {
    if (!tauriReady || repairStatusRefreshing) return;
    repairStatusRefreshing = true;
    try {
      repairStatus = await core.invoke("get_daemon_repair_status");
    } catch {
      // Keep the last known repair state across transient startup failures.
    } finally {
      repairStatusRefreshing = false;
    }
  }

  async function refreshStratumStatus() {
    if (!tauriReady) return;
    try {
      const status = await core.invoke("get_stratum_status");
      stratumStatus.set(status);
    } catch {
      // Stratum status is optional; keep the last known state.
    }
  }

  async function refreshVaultStatus() {
    if (!tauriReady) return;
    try {
      const status = await core.invoke("ipfs_vault_unlock_status");
      vaultExists = !!status?.vault_exists;
      vaultLocked = !status?.unlocked;
      vaultStatus.set({ exists: vaultExists, unlocked: !vaultLocked });
    } catch {
      vaultExists = false;
      vaultLocked = true;
      vaultStatus.set({ exists: false, unlocked: false });
    }
  }

  function openVaultUnlockModal() {
    vaultUnlockPassphrase = "";
    vaultUnlockError = "";
    showVaultUnlockModal = true;
  }

  function closeVaultUnlockModal() {
    showVaultUnlockModal = false;
    vaultUnlockPassphrase = "";
    vaultUnlockError = "";
  }

  async function confirmVaultUnlock() {
    if (!tauriReady || !vaultUnlockPassphrase) return;
    vaultUnlocking = true;
    vaultUnlockError = "";
    try {
      const ok = await core.invoke("ipfs_unlock_vault", { passphrase: vaultUnlockPassphrase });
      if (ok) {
        vaultLocked = false;
        vaultStatus.set({ exists: true, unlocked: true });
        closeVaultUnlockModal();
      } else {
        vaultUnlockError = "Incorrect passphrase.";
      }
    } catch (err) {
      vaultUnlockError = String(err || "Failed to unlock vault");
    }
    vaultUnlocking = false;
  }

  async function handleVaultStatusClick() {
    if (!tauriReady) return;
    await refreshVaultStatus();
    if (!vaultExists) {
      activeTab = "TOOLS";
      window.dispatchEvent(new CustomEvent("commander-open-tools-wallet"));
      return;
    }
    if (vaultLocked) {
      openVaultUnlockModal();
    } else {
      try {
        await core.invoke("ipfs_lock_vault");
        vaultLocked = true;
        vaultStatus.set({ exists: true, unlocked: false });
      } catch {}
    }
  }

  /**
   * @param {string} status
   */
  function walletActionLabel(status) {
    if (status === "UNENCRYPTED") {
      return "ENCRYPT WALLET";
    }
    if (status === "LOCKED") {
      return "UNLOCK WALLET";
    }
    return "LOCK WALLET";
  }

  /**
   * @param {string} mode
   */
  function openWalletPrompt(mode) {
    walletPromptMode = mode;
    walletPromptPass = "";
    walletPromptPassConfirm = "";
    walletPromptDuration = "300";
    walletPromptError = "";
    walletPromptPin = "";
    walletPromptMode2 = "passphrase";
    walletPinStatus = null;
    showWalletPrompt = true;
    if (mode === "unlock") {
      loadWalletPinStatus().then((status) => {
        if (showWalletPrompt && walletPromptMode === "unlock") {
          walletPinStatus = status;
          if (walletPromptMode2 === "passphrase") {
            walletPromptMode2 = defaultUnlockMode(status);
          }
        }
      });
    }
  }

  function closeWalletPrompt() {
    showWalletPrompt = false;
    walletPromptPin = "";
  }

  function switchWalletPromptToPassphrase() {
    walletPromptMode2 = "passphrase";
    walletPromptPin = "";
    walletPromptError = "";
  }

  function switchWalletPromptToPin() {
    if (!walletPinUsable) return;
    walletPromptMode2 = "pin";
    walletPromptPin = "";
    walletPromptError = "";
  }

  async function forgotWalletPromptPin() {
    if (walletPromptWorking) return;
    walletPromptWorking = true;
    walletPromptError = "";
    try {
      await forgotWalletPin();
      walletPinStatus = await loadWalletPinStatus();
      walletPromptMode2 = "passphrase";
      walletPromptPin = "";
      walletPromptError = "Device PIN cleared. Enter your wallet passphrase to unlock.";
    } catch (err) {
      walletPromptError = "Could not clear PIN: " + String(err);
    } finally {
      walletPromptWorking = false;
    }
  }

  async function confirmWalletPrompt() {
    if (!tauriReady) return;
    // PIN unlock mode (only for unlock, not encrypt).
    if (walletPromptMode === "unlock" && walletPromptMode2 === "pin" && walletPinUsable) {
      if (!isValidPin(walletPromptPin)) {
        walletPromptError = "Enter your 6-digit PIN.";
        return;
      }
      walletPromptWorking = true;
      walletPromptError = "";
      try {
        const duration = Number(walletPromptDuration || 300);
        await unlockWalletWithPin(walletPromptPin, duration);
        walletPromptPin = "";
        showWalletPrompt = false;
        try { networkMode = await core.invoke("get_network_mode"); } catch (e) {}
        await refreshDashboard();
      } catch (err) {
        walletPromptPin = "";
        walletPromptError = String(err || "PIN unlock failed");
        walletPinStatus = await loadWalletPinStatus();
        if (pinRequiresPassphrase(walletPinStatus)) {
          walletPromptMode2 = "passphrase";
        }
      } finally {
        walletPromptWorking = false;
      }
      return;
    }
    try {
      if (walletPromptMode === "encrypt") {
        if (!walletPromptPass) {
          walletPromptError = "Password required.";
          return;
        }
        if (walletPromptPass !== walletPromptPassConfirm) {
          walletPromptError = "Passwords do not match.";
          return;
        }
        await core.invoke("wallet_encrypt", { password: walletPromptPass });
      } else if (walletPromptMode === "unlock") {
        if (!walletPromptPass) {
          walletPromptError = "Password required.";
          return;
        }
        const duration = Number(walletPromptDuration || 60);
        await unlockRuntimeWalletWithPassphrase(walletPromptPass, duration);
        
        // Simultaneously unlock the Ravencoin wallet using the same password
        try {
          await core.invoke("rvn_wallet_unlock", { passphrase: walletPromptPass, timeout: duration });
        } catch (e) {
          console.warn("Ravencoin wallet unlock skipped or failed:", e);
        }

        // After a successful full passphrase unlock, offer to set a device PIN
        // if none is configured. The offer is surfaced via the wallet prompt's
        // success path; the Wallet page also shows Set/Change/Remove PIN.
        walletPinStatus = await loadWalletPinStatus();
      }
      // Check Network Mode
      try {
        networkMode = await core.invoke("get_network_mode");
      } catch (e) {
        console.error("Failed to get network mode", e);
      }

      closeWalletPrompt();
      await refreshDashboard();
    } catch (err) {
      walletPromptError = String(err || "Wallet action failed");
    }
  }

  async function handleWalletAction() {
    if (!tauriReady) return;
    try {
      if (walletInfo.status === "UNENCRYPTED") {
        openWalletPrompt("encrypt");
      } else if (walletInfo.status === "LOCKED") {
        openWalletPrompt("unlock");
      } else {
        await core.invoke("wallet_lock");
        await refreshDashboard();
      }
    } catch (err) {
      lastError = String(err || "Wallet action failed");
    }
  }

  /**
   * After Core is running, verify the active vault wallet (if any) is
   * actually loaded/queryable. Surface a clear, actionable notification
   * when it is not, so the user is never silently left on the wrong/no
   * wallet after start/auto-start. Never throws.
   */
  async function verifyActiveVaultWalletLoaded() {
    try {
      const startup = await core.invoke("vault_get_active_wallet_startup_state");
      if (startup?.active_wallet_name && !startup?.wallet_queryable) {
        const name = startup.active_wallet_name;
        lastError = startup.load_error
          || `Active vault wallet "${name}" is not loaded in Core. Use the Wallet page to restore/load it, or restart Core through Commander.`;
        addRuntimeNotification("Vault wallet not loaded", lastError, "warning");
        return false;
      }
      return true;
    } catch (e) {
      // Probe is best-effort; do not block startup on it.
      return true;
    }
  }

  function isCoreLockBusyMessage(err) {
    const msg = String(err || "");
    return msg.includes("CORE_LOCK_BUSY::") || msg.includes("Cannot obtain a lock on data directory");
  }

  async function waitForDaemonStopped(timeoutMs = 45000) {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      await refreshDashboard();
      const status = get(nodeStatus);
      if (!status.online && nodeInfo.state !== "RUNNING") {
        return true;
      }
      await sleep(750);
    }
    return false;
  }

  async function handleStart() {
    if (!tauriReady || daemonOperation !== "idle") return;
    if (daemonStatusClearTimer) {
      clearTimeout(daemonStatusClearTimer);
      daemonStatusClearTimer = null;
    }
    daemonOperation = "starting";
    daemonStatusMessage = "Starting";
    daemonPollProgress = "";
    addRuntimeNotification("Daemon start requested", "", "info");
    try {
      await core.invoke("start_node");
      await core.invoke("take_daemon_ownership");
      daemonRuntime.update((d) => ({ ...d, commanderOwns: true }));
      daemonStatusMessage = "Starting";
      const readiness = await core.invoke("wait_for_daemon_ready", { timeoutMs: 90000 });
      daemonRuntime.update((d) => ({ ...d, readiness }));
      daemonPollProgress = readiness.ready
        ? `Ready in ${(readiness.elapsed_ms / 1000).toFixed(1)}s (${readiness.retries} polls)`
        : "Core is still starting. Commander will keep checking status.";
      if (!readiness.ready) {
        daemonStatusMessage = "Still starting";
        lastError = readiness.rpc_error || "Core has not answered RPC yet.";
        try {
          const status = await core.invoke("get_daemon_repair_status");
          const hint = status?.log_hint || status?.phase || "";
          if (hint && hint !== "Idle") {
            daemonPollProgress = hint;
          }
        } catch {
          // Best-effort startup hint only.
        }
        if (!/connection failed|actively refused|no cookie file|rpc authentication unavailable/i.test(lastError)) {
          addRuntimeNotification("Daemon still starting", lastError, "warning");
        }
      } else {
        daemonStatusMessage = "Ready";
        lastDaemonStartSuccessAt = Date.now();
        await refreshRpcAuthStatus();
        await refreshDashboard();
        addRuntimeNotification("Daemon started", "", "success");
        // Verify the active vault wallet is loaded; warn if not.
        await verifyActiveVaultWalletLoaded();
      }
      setTimeout(refreshDashboard, 1500);
    } catch (err) {
      lastError = String(err || "Failed to start node");
      if (isCoreLockBusyMessage(lastError)) {
        daemonStatusMessage = "Core busy";
        daemonPollProgress = "Try again in a few moments.";
        addRuntimeNotification("Core is still settling", daemonPollProgress, "warning");
        setTimeout(refreshDashboard, 1500);
      } else {
        addRuntimeNotification("Daemon start failed", lastError, "error");
      }
    } finally {
      daemonOperation = "idle";
      daemonStatusClearTimer = setTimeout(() => {
        daemonStatusMessage = "";
        daemonPollProgress = "";
        daemonStatusClearTimer = null;
      }, 5000);
    }
  }

  async function handleStop() {
    if (!tauriReady || daemonOperation !== "idle") return;
    if (daemonStatusClearTimer) {
      clearTimeout(daemonStatusClearTimer);
      daemonStatusClearTimer = null;
    }
    daemonOperation = "stopping";
    daemonStatusMessage = "Stopping";
    daemonPollProgress = "";
    addRuntimeNotification("Daemon stop requested", "", "info");
    try {
      await core.invoke("stop_node_and_wait", { timeoutMs: 90000 });
      await core.invoke("release_daemon_ownership");
      daemonRuntime.update((d) => ({ ...d, commanderOwns: false }));
      daemonStatusMessage = "Stopping";
      const stopped = await waitForDaemonStopped();
      await refreshRpcAuthStatus();
      if (stopped) {
        addRuntimeNotification("Daemon stopped", "", "info");
        daemonStatusMessage = "Stopped";
      } else {
        daemonStatusMessage = "Stopping";
        daemonPollProgress = "Core is still shutting down. Status will update shortly.";
        addRuntimeNotification("Daemon still stopping", daemonPollProgress, "warning");
        setTimeout(refreshDashboard, 1500);
      }
    } catch (err) {
      lastError = String(err || "Failed to stop node");
      addRuntimeNotification("Daemon stop failed", lastError, "error");
    } finally {
      daemonOperation = "idle";
      daemonStatusClearTimer = setTimeout(() => {
        daemonStatusMessage = "";
        daemonPollProgress = "";
        daemonStatusClearTimer = null;
      }, 5000);
    }
  }

  onMount(() => {
    tauriReady = true; // Force accurate ready state for UI logic
    systemStore.update((s) => ({ ...s, tauriReady: true }));

    if (!hempSnapshotInstalled) {
      core.invoke("snapshot_get_info", { chain: "hemp0x" }).then((info) => {
        if (info && (info.installed || info.has_database)) {
          hempSnapshotInstalled = true;
          localStorage.setItem("hemp0x_snapshot_installed", "true");
        }
      }).catch(() => {});
    }

    // Initialize Nostr P2P network
    initNostr().catch(e => console.error("Nostr init failed:", e));

    // Show window unconditionally (Anti-Flash)
    setTimeout(async () => {
      try {
        await getCurrentWindow().show();
        await getCurrentWindow().setFocus();
      } catch (e) {
        console.warn("Could not show window:", e);
      }
    }, 150);
    sessionStamp = new Date().toISOString().replace("T", " ").slice(0, 19);
    const updateScale = () => {
      const w = window.innerWidth || 1080;
      const h = window.innerHeight || 720;

      // FIXED RATIO ZOOM: Everything locked in place.
      // Acts like resizing an image. Base resolution: 1280x800.
      let ratio = Math.min(w / 1280, h / 800);

      // No minimum floor - allows infinite shrinking to prevent cutoff.
      // Cap at 1.0 to ensure sharpness.
      ratio = Math.min(ratio, 1.0);

      uiScale = Number(ratio.toFixed(4));
    };
    updateScale();
    window.addEventListener("resize", updateScale);

    /** @type {(() => void) | undefined} */
    let unlistenNetwork;
    let unlistenHistoryRecovery;
    const openLibraryHandler = () => {
      activeTab = "TOOLS";
      ipfsHubSection.set("library");
    };
    window.addEventListener("commander-open-content-library", openLibraryHandler);
    const openExplorerHandler = () => {
      activeTab = "TOOLS";
    };
    window.addEventListener("commander-open-explorer", openExplorerHandler);
    const openAssetBrowserHandler = () => {
      activeTab = "ASSETS";
    };
    window.addEventListener("commander-open-asset-browser", openAssetBrowserHandler);
    const vaultUnlockHandler = () => {
      openVaultUnlockModal();
    };
    window.addEventListener("commander-open-vault-unlock", vaultUnlockHandler);
    // Route Tools > System requests (e.g. H0XC chat "Open System Config") to
    // the TOOLS tab. ViewTools listens for the same event and switches its
    // inner SYSTEM sub-tab; this handler ensures the top-level tab also moves
    // so the user actually lands on System when the request originates from a
    // non-TOOLS view (e.g. the ASSETS-tab asset detail / H0XC chat).
    const openToolsSystemHandler = () => {
      activeTab = "TOOLS";
    };
    window.addEventListener("commander-open-tools-system", openToolsSystemHandler);
    if (tauriReady) {
      // Handle close event for daemon lifecycle
      getCurrentWindow().onCloseRequested(async (event) => {
        if (closeCleanupComplete) {
          return;
        }
        event.preventDefault();
        if (closeCleanupInProgress) {
          return;
        }

        let owns = false;
        try {
          owns = (await core.invoke("get_daemon_ownership")).commander_owns;
        } catch {
          // can't determine ownership, close immediately
        }

        const status = get(nodeStatus);
        if (owns || status.online) {
          closeCleanupInProgress = true;
          closePromptStatus = "";
          showClosePrompt = true;
        } else {
          closePromptStatus = "Closing Commander...";
          await exitCommanderProcess();
        }
      });

      // Init daemon runtime
      (async function initDaemonRuntime() {
        try {
          await loadAppSettings();
          if (rpcAuthStatus?.auth_mode === 'legacy_userpass') {
            addRuntimeNotification("Legacy RPC auth", rpcAuthStatus.warning, "warning");
          }
          const status = await core.invoke("get_runtime_status");
          daemonRuntime.update((d) => ({
            ...d,
            bundledCoreNextReady: status.bundled_core_next_ready,
            probe: status.probe,
            daemon: {
              path: status.daemon.path,
              exists: status.daemon.exists,
              raw: status.daemon.raw,
              base_version: status.daemon.base_version,
              commit_hash: status.daemon.commit_hash,
              exact_core_next_match: status.daemon.exact_core_next_match,
            },
          }));

          if (status.bundled_core_next_ready && !coreNextReadyNotified) {
            coreNextReadyNotified = true;
            const hash = status.daemon.commit_hash || "unknown";
            addRuntimeNotification("Core Next ready", `${hash}`, "success");
          }

          if (!status.bundled_core_next_ready && status.daemon.exists && !coreNextMismatchNotified) {
            coreNextMismatchNotified = true;
            const bundled = status.daemon.base_version
              ? `v${status.daemon.base_version}${status.daemon.commit_hash ? ` (${status.daemon.commit_hash})` : ""}`
              : "unrecognized";
            addRuntimeNotification(
              "Core Next version mismatch",
              `Bundled: ${bundled}. Required: ${status.required_base_version}-${status.required_commit_hash}.`,
              "warning",
            );
          }

          if (status.probe.rpc_port_open) {
            const conflict = status;
            conflictRuntimeStatus = conflict;
            try {
              const identity = await core.invoke("identify_running_daemon", {
                allowNonBundled: appSettings.allow_non_bundled_core_next,
              });
              daemonRuntime.update((d) => ({ ...d, runningIdentity: identity }));
              conflict.identity = identity;
            } catch {
              // identity probe is best-effort
            }
            /** @type {any} */
            let procId = null;
            try {
              procId = await core.invoke("get_daemon_process_identity");
              daemonRuntime.update((d) => ({ ...d, processIdentity: procId }));
              if (procId.available) {
                conflict.processIdentity = procId;
              }
            } catch {
              // process identity is best-effort
            }

            const exactMatch = procId?.available
              && procId?.sha256_match
              && procId?.version_commit_match;
            const compatibleCoreNextRpc =
              conflict.identity?.rpc_authenticated
              && conflict.identity?.base_version === status.required_base_version
              && conflict.identity?.capabilities?.help_probe_success
              && status.bundled_core_next_ready;

            if (exactMatch) {
              addRuntimeNotification(
                "Bundled daemon verified",
                "Exact Core Next match confirmed via process identity.",
                "success",
              );
              conflictResolved = true;
              daemonRuntime.update((d) => ({ ...d, conflictResolved: true }));
            } else if (compatibleCoreNextRpc) {
              addRuntimeNotification(
                "Core Next daemon detected",
                "Compatible Core Next capabilities confirmed over RPC.",
                "success",
              );
              conflictResolved = true;
              daemonRuntime.update((d) => ({ ...d, conflictResolved: true }));
            } else {
              addRuntimeNotification(
                "External daemon detected",
                `An existing daemon is running on port ${status.probe.default_rpc_port}.`,
                "warning",
              );
              showDaemonConflict = true;
            }
          } else if (appSettings.auto_start_daemon_on_launch && status.bundled_core_next_ready) {
            addRuntimeNotification("Daemon start requested (auto)", "", "info");
            try {
              await core.invoke("start_node");
              await core.invoke("take_daemon_ownership");
              daemonRuntime.update((d) => ({ ...d, commanderOwns: true }));
              const readiness = await core.invoke("wait_for_daemon_ready", { timeoutMs: 30000 });
              daemonRuntime.update((d) => ({ ...d, readiness }));
              if (!readiness.ready) {
                lastError = "Daemon did not become ready after start: " + (readiness.rpc_error || "timeout");
                addRuntimeNotification("Daemon readiness failed", lastError, "error");
              } else {
                await refreshRpcAuthStatus();
                lastDaemonStartSuccessAt = Date.now();
                addRuntimeNotification("Daemon started (auto)", "", "success");
                // Verify the active vault wallet is loaded after auto-start.
                await verifyActiveVaultWalletLoaded();
              }
              conflictResolved = true;
              daemonRuntime.update((d) => ({ ...d, conflictResolved: true }));
            } catch (e) {
              console.error("Failed to auto-start daemon:", e);
              addRuntimeNotification("Daemon auto-start failed", String(e).substring(0, 200), "error");
            }
          } else {
            conflictResolved = true;
            daemonRuntime.update((d) => ({ ...d, conflictResolved: true }));
          }

          if (appSettings.allow_non_bundled_core_next === false &&
              !status.bundled_core_next_ready &&
              status.daemon.exists) {
            console.warn("Bundled Core Next is not the exact required version");
          }
        } catch (e) {
          console.error("Daemon runtime init failed:", e);
          addRuntimeNotification("Runtime init failed", String(e).substring(0, 200), "error");
          conflictResolved = true;
          daemonRuntime.update((d) => ({ ...d, conflictResolved: true }));
        }
      })();

      // Load Network Mode
      core
        .invoke("get_network_mode")
        .then((res) => {
          networkMode = res;
          networkInfo.update((n) => ({ ...n, chain: res }));
        })
        .catch((err) => {
          console.error("Failed to load network mode", err);
        });

      // Listen for runtime changes
      listen("network-changed", (event) => {
        if (event.payload && event.payload.mode) {
          networkMode = event.payload.mode;
          networkInfo.update((n) => ({ ...n, chain: event.payload.mode }));
        }
      }).then((fn) => {
        unlistenNetwork = fn;
      });

      listen("wallet-history-recovery-complete", async (event) => {
        coreBusyUntil.set(0);
        await refreshDashboard();
        if (event.payload?.success) {
          addRuntimeNotification(
            "Wallet history recovered",
            `Balance and transactions refreshed for ${event.payload.wallet_name || "the active wallet"}.`,
            "success",
          );
        } else {
          addRuntimeNotification(
            "Wallet history recovery failed",
            String(event.payload?.error || "Core did not complete the wallet history scan."),
            "error",
          );
        }
      }).then((fn) => {
        unlistenHistoryRecovery = fn;
      });
    }
    checkWelcomePopup(); // Show welcome popup if enabled

    // Adaptive Polling Logic for Performance
    const performPoll = async () => {
      await refreshRepairStatus();
      if (Date.now() < coreBusyUntilMs) {
        timer = setTimeout(performPoll, repairActive ? 3000 : 5000);
        return;
      }
      if (conflictResolved) {
        if (!repairActive || repairStatus?.rpc_online) {
          await refreshDashboard();
        }
        await refreshStratumStatus();
        await refreshRpcAuthStatus();
        await refreshVaultStatus();
      }

      let delay = repairActive ? 3000 : 5000;
      if (systemStatus === "yellow") delay = 8000;
      else if (systemStatus === "red") delay = 5000;

      timer = setTimeout(performPoll, delay);
    };

    let timer = setTimeout(performPoll, 100);

    return () => {
      clearTimeout(timer);
      unsubscribeCoreBusy();
      if (typeof unlistenHistoryRecovery === "function") unlistenHistoryRecovery();
      window.removeEventListener("resize", updateScale);
      window.removeEventListener("commander-open-content-library", openLibraryHandler);
      window.removeEventListener("commander-open-explorer", openExplorerHandler);
      window.removeEventListener("commander-open-asset-browser", openAssetBrowserHandler);
      window.removeEventListener("commander-open-vault-unlock", vaultUnlockHandler);
      window.removeEventListener("commander-open-tools-system", openToolsSystemHandler);
      if (typeof unlistenNetwork === "function") unlistenNetwork();
    };
  });
</script>

<main class="shell" style={`--ui-scale: ${uiScale};`}>
  <!-- HEADER -->
  <header class="top-bar">
    <div class="brand">
      <img src={logoNew} alt="Hemp0x" class="logo" />
      <div class="brand-info">
        <h1 class="app-title">
          HEMP0X COMMANDER <span class="version">{APP_VERSION}</span>
        </h1>
        <div class="app-status">SECURE SESSION - {sessionStamp}</div>
      </div>
    </div>

    <nav class="main-nav">
      {#each ["DASHBOARD", "SEND", "RECEIVE", "ASSETS", "TOOLS", "ABOUT"] as tab}
        <button
          class="tab-btn"
          class:active={isActive(tab)}
          type="button"
          on:click={() => setTab(tab)}
        >
          {tab}
        </button>
      {/each}
      
      <div class="nav-divider" style="width: 1px; height: 20px; background: rgba(255,255,255,0.2); margin: 0 10px; align-self: center;"></div>
      
      <button
        class="tab-btn"
        class:active={isActive('RVN_DASHBOARD')}
        type="button"
        style={isActive('RVN_DASHBOARD') ? "color: #ff6b00; border-bottom: 2px solid #ff6b00;" : ""}
        on:click={() => setTab('RVN_DASHBOARD')}
      >
        <span style="color: #ff6b00; margin-right: 4px;">RVN</span>DASHBOARD
      </button>

    </nav>

    <div class="window-controls">
      <NotificationCenter />
      <div class="status-stack">
        <div class="traffic-lights">
          <!-- Status Traffic Lights -->
          <span
            class="status-dot green"
            class:active={systemStatus === "green"}
            title="All Systems Go"
          ></span>
          <span
            class="status-dot yellow"
            class:active={systemStatus === "yellow"}
            title="Syncing/Partial"
          ></span>
          <span
            class="status-dot red"
            class:active={systemStatus === "red"}
            title="Node Offline"
          ></span>
        </div>
        {#if networkMode !== "mainnet"}
          <div class="network-warning-text">
            {networkMode.toUpperCase()}
          </div>
        {/if}
      </div>
    </div>
  </header>

  <!-- TRUST SIGNAL STRIP -->
  <div class="trust-strip">
    <div
      class="ts-item"
      class:ts-ok={coreNextOk}
      class:ts-bad={coreNextKnown && !coreNextOk}
    >
      <span class="ts-label">Core Next</span>
      <span class="ts-val">{coreNextOk ? "MATCH" : coreNextVersion}</span>
    </div>
    <div
      class="ts-item"
      class:ts-ok={nodeInfo.state === "RUNNING"}
      class:ts-bad={nodeInfo.state !== "RUNNING" && nodeInfo.state !== "--"}
    >
      <span class="ts-label">Daemon</span>
      <span class="ts-val">{nodeInfo.state}</span>
    </div>
    <button
      type="button"
      class="ts-item"
      class:ts-ok={walletInfo.status !== "UNENCRYPTED" &&
        walletInfo.status !== "LOCKED" &&
        walletInfo.status !== "--"}
      class:ts-warn={walletInfo.status === "LOCKED"}
      class:ts-bad={walletInfo.status === "UNENCRYPTED"}
      class:wallet-status-action={walletInfo.status !== "--" && nodeInfo.state === "RUNNING"}
      disabled={walletInfo.status === "--" || nodeInfo.state !== "RUNNING"}
      title={nodeInfo.state === "RUNNING" ? walletActionLabel(walletInfo.status) : "Wallet controls require the daemon to be running"}
      on:click={handleWalletAction}
    >
      <span class="ts-label">Wallet</span>
      <span class="ts-val">{walletInfo.status}</span>
    </button>
    <button
      type="button"
      class="ts-item"
      class:ts-ok={vaultExists && !vaultLocked}
      class:ts-warn={vaultExists && vaultLocked}
      class:wallet-status-action={vaultExists}
      title={vaultExists ? (vaultLocked ? "Unlock Vault" : "Lock Vault") : "No vault configured"}
      on:click={handleVaultStatusClick}
    >
      <span class="ts-label">Vault</span>
      <span class="ts-val">
        {#if !vaultExists}
          NONE
        {:else if vaultLocked}
          LOCKED
        {:else}
          UNLOCKED
        {/if}
      </span>
    </button>
    <div class="ts-item">
      <span class="ts-label">Network</span>
      <span class="ts-val">LOCAL ONLY</span>
    </div>
    <button
      type="button"
      class="ts-item"
      class:ts-ok={rpcAuthStatus?.auth_mode === 'cookie'}
      class:ts-bad={rpcAuthStatus && rpcAuthStatus.auth_mode !== 'cookie'}
      class:wallet-status-action={rpcAuthStatus?.auth_mode !== 'cookie'}
      title={rpcAuthStatus?.auth_mode === 'cookie' ? 'Cookie RPC authentication is active' : 'Open RPC cookie auth guidance'}
      on:click={openRpcAuthHelp}
    >
      <span class="ts-label">RPC Auth</span>
      <span class="ts-val">{rpcAuthStatus ? (rpcAuthStatus.auth_mode === 'cookie' ? 'Cookie' : rpcAuthStatus.auth_mode === 'legacy_userpass' ? 'Legacy' : '--') : '--'}</span>
    </button>
    <div
      class="ts-item"
      class:ts-ok={stratumRunning}
      class:ts-warn={stratumState.state === "STARTING" || stratumState.state === "STOPPING"}
      class:ts-bad={stratumState.state === "ERROR"}
    >
      <span class="ts-label">Stratum</span>
      <span class="ts-val">{stratumState.state}</span>
    </div>
  </div>

  <!-- CONTENT AREA -->
  <div class="content" class:no-padding={activeTab === "TOOLS"}>
    <!-- DASHBOARD VIEW -->
    <!-- DASHBOARD VIEW -->
    <div class="view-wrapper" class:show={activeTab === "DASHBOARD"}>
      <div class="view-dashboard">
        <!-- TOP ROW -->
        <div class="row-top">
          <!-- NODE PANEL -->
          <div class="glass-panel node-card cyber-panel">
            <header class="panel-header">
              <span class="hud-title mono">[ NODE STATUS ]</span>
              <div class="stat-pill">
                <span
                  class="led"
                  class:running={nodeInfo.state === "RUNNING"}
                  class:stopped={nodeInfo.state !== "RUNNING"}
                ></span>
                <span class="value"
                  >{nodeInfo.state === "RUNNING" ? "RUNNING" : "STOPPED"}</span
                >
              </div>
            </header>

            <div class="panel-content">
              <div class="stat-grid-compact">
                <div class="stat-pair">
                  <span class="label">SYNC</span>
                  <span
                    class="mono stat-state"
                    class:state-ok={nodeInfo.synced && !repairActive}
                    class:state-warn={repairActive || (!nodeInfo.synced &&
                      nodeInfo.state === "RUNNING")}
                  >
                    {syncLabel}
                  </span>
                </div>
                <div class="stat-pair">
                  <span class="label">BLOCK</span>
                  <span class="mono">{nodeInfo.blocks}</span>
                </div>
                <div class="stat-pair">
                  <span class="label">PEERS</span>
                  <span class="mono">{nodeInfo.peers}</span>
                </div>
                <div class="stat-pair">
                  <span class="label">DIFF</span>
                  <span class="mono">{nodeInfo.diff}</span>
                </div>
              </div>
              {#if repairActive}
                <div class="repair-progress" aria-live="polite">
                  <div class="repair-progress-head">
                    <span>{repairDisplayPhase}</span>
                    {#if repairProgress != null}
                      <span>{repairProgress.toFixed(2)}%</span>
                    {/if}
                  </div>
                  <div class="repair-progress-track">
                    <span
                      class:indeterminate={repairProgress == null}
                      style={repairProgress == null ? "" : `width: ${repairProgress}%`}
                    ></span>
                  </div>
                  {#if repairStatus.blocks != null && repairStatus.headers != null}
                    <div class="repair-progress-detail">
                      {repairStatus.blocks} / {repairStatus.headers} blocks
                    </div>
                  {/if}
                </div>
              {/if}
            </div>

            <div class="panel-actions">
              <button
                class="btn-xs"
                on:click={handleStart}
                disabled={daemonOperation !== "idle"}
              >START</button>
              <button
                class="btn-xs ghost"
                on:click={handleStop}
                disabled={daemonOperation !== "idle"}
              >STOP</button>
              {#if !hempSnapshotInstalled && !nodeInfo.synced}
                <button
                  class="btn-xs"
                  style="border-color: #00e676; color: #00e676; background: rgba(0, 230, 118, 0.15); font-weight: bold;"
                  on:click={() => (showHempFastSyncModal = true)}
                >⚡ FAST SYNC</button>
              {/if}
              {#if daemonOperation !== "idle"}
                <span
                  class="daemon-loader"
                  title={daemonPollProgress || (daemonOperation === "starting" ? "Waiting for daemon to become ready" : "Stopping daemon")}
                >
                  <CommanderLoader compact={true} label="" detail="" />
                  <span class="daemon-loader-label">{daemonOperation === "starting" ? "STARTING" : "STOPPING"}</span>
                </span>
              {:else if daemonStatusMessage}
                <span class="daemon-status-text" title={daemonPollProgress || daemonStatusMessage}>{daemonStatusMessage}</span>
              {/if}
            </div>
          </div>

          {#if showHempFastSyncModal}
            <FastSyncModal chain="hemp0x" on:close={() => (showHempFastSyncModal = false)} on:complete={() => { hempSnapshotInstalled = true; localStorage.setItem("hemp0x_snapshot_installed", "true"); refreshDashboard(); }} />
          {/if}

          <!-- WALLET PANEL -->
          <div class="glass-panel wallet-card cyber-panel">
            <header class="panel-header">
              <div class="header-left">
                <span class="hud-title mono">[ WALLET ]</span>
                <button
                  class="btn-eye"
                  title={hideBalance ? "Show Balance" : "Hide Balance"}
                  on:click={toggleHideBalance}
                >
                  <img src={hideBalance ? eyeClosed : eyeOpen} alt="toggle" />
                </button>
              </div>
              <button
                type="button"
                class="status-chip"
                class:status-red={walletInfo.status === "UNENCRYPTED"}
                class:status-green={walletInfo.status !== "UNENCRYPTED" &&
                  walletInfo.status !== "--" && walletInfo.status !== "LOCKED"}
                class:status-warn={walletInfo.status === "LOCKED"}
                disabled={nodeInfo.state !== "RUNNING" || walletInfo.status === "--"}
                title={nodeInfo.state === "RUNNING" ? walletActionLabel(walletInfo.status) : "Wallet controls require the daemon to be running"}
                on:click={handleWalletAction}
              >
                {walletInfo.status}
              </button>
            </header>

            <div class="panel-content wallet-content compact-wallet">
              <div class="balance-hero-small">
                <div class="val neon-glow" class:blurred={hideBalance}>
                  {hideBalance ? "******" : walletInfo.balance}
                  <span class="unit">HEMP</span>
                </div>
                <div class="sub">AVAILABLE BALANCE</div>
              </div>
              <div class="wallet-metrics" class:blurred={hideBalance}>
                {#if walletInfo.pending !== '--'}
                  <span class="metric">PENDING: {walletInfo.pending} HEMP</span>
                {/if}
                {#if walletInfo.staked !== '--' && parseFloat(walletInfo.staked) > 0}
                  <span class="metric">IMMATURE: {walletInfo.staked} HEMP</span>
                {/if}
              </div>
            </div>

            <div class="panel-actions wallet-actions">
              <button
                class="btn-xs"
                class:disabled={nodeInfo.state !== "RUNNING" ||
                  walletInfo.status === "--"}
                disabled={nodeInfo.state !== "RUNNING" ||
                  walletInfo.status === "--"}
                on:click={handleWalletAction}
              >
                {nodeInfo.state !== "RUNNING"
                  ? "NOT CONNECTED"
                  : walletActionLabel(walletInfo.status)}
              </button>
            </div>
          </div>
        </div>

        <!-- BOTTOM ROW: ACTIVITY -->
        <div class="row-bottom">
          <div
            class="glass-panel activity-card cyber-panel"
            class:expanded={isActivityExpanded}
          >
            <header class="panel-header">
              <div class="header-left">
                <span class="hud-title mono">[ RECENT ACTIVITY ]</span>
                <button
                  class="btn-eye"
                  title={hideActivity ? "Show Activity" : "Hide Activity"}
                  on:click={toggleHideActivity}
                >
                  <img src={hideActivity ? eyeClosed : eyeOpen} alt="toggle" />
                </button>
                <button
                  class="btn-eye btn-expand"
                  title={isActivityExpanded ? "Collapse" : "Expand Full Screen"}
                  on:click={toggleActivityExpand}
                >
                  <!-- Simple ^ chevron or box icon -->
                  <span class="expand-icon"
                    >{isActivityExpanded ? "▼" : "▲"}</span
                  >
                </button>
                <button
                  class="activity-history-btn"
                  type="button"
                  title="Open full History page"
                  on:click={openHistoryPage}
                >
                  <span class="activity-history-icon" aria-hidden="true">&#8635;</span>
                  <span class="activity-history-label">HISTORY</span>
                </button>
              </div>
              <span class="hint mono">LAST 50 TRANSACTIONS</span>
            </header>

            <div class="header-row">
              <span>Date</span>
              <span>Type</span>
              <span>Amount</span>
              <span>Conf</span>
              <span>TXID</span>
            </div>

            <div
              class="scroll-body custom-scroll"
              class:activity-mask={hideActivity}
            >
              {#each recentTx as tx}
                <div class="data-row">
                  <span class="mono dim">{tx.date}</span>
                  <span class="type {tx.type}">{tx.type}</span>
                  <span
                    class="mono amount {String(tx.amount).startsWith('-')
                      ? 'neg'
                      : 'pos'}"
                  >
                    {tx.amount}
                  </span>
                  <span class="mono dim">{tx.conf}</span>
                  <span class="recent-tx-actions">
                    <span class="mono txid" title={tx.txid}>{tx.txid}</span>
                    <button
                      class="activity-icon-btn"
                      type="button"
                      title="Explore transaction"
                      aria-label="Explore transaction"
                      on:click={() => openExplorer(tx.txid)}
                    >
                      &#x2315;
                    </button>
                    <button
                      class="activity-icon-btn"
                      type="button"
                      title="Copy transaction ID"
                      aria-label="Copy transaction ID"
                      on:click={() => copyRecentTxid(tx.txid)}
                    >
                      <CopyIcon />
                    </button>
                  </span>
                </div>
              {/each}
            </div>
          </div>
        </div>
      </div>
    </div>

    <div class="view-wrapper" class:show={activeTab === "SEND"}>
      <ViewSend />
    </div>

    <div class="view-wrapper" class:show={activeTab === "RECEIVE"}>
      <ViewReceive />
    </div>

    <div class="view-wrapper" class:show={activeTab === "ASSETS"}>
      <ViewAssets />
    </div>

    <div class="view-wrapper" class:show={activeTab === "TOOLS"}>
      <ViewTools
        bind:consoleOutput={globalConsoleOutput}
        bind:consoleHistory={globalConsoleHistory}
      />
    </div>

    <div class="view-wrapper" class:show={activeTab === "ABOUT"}>
      <div class="about-page glass-panel panel-strong">
        <div class="about-header">
          <img src={logoNew} alt="Hemp0x Logo" class="about-logo" />
          <div class="about-title-block">
            <h1 class="about-title">HEMP0X COMMANDER</h1>
            <span class="about-version">{APP_VERSION}</span>
          </div>
          <label class="welcome-toggle">
            <input
              type="checkbox"
              bind:checked={showWelcomeOnStartup}
              on:change={() =>
                localStorage.setItem(
                  "hemp0x_showWelcome",
                  showWelcomeOnStartup ? "1" : "0",
                )}
            />
            <span>Show welcome on startup</span>
          </label>
        </div>

        <div class="about-section">
          <h3 class="section-header">ABOUT</h3>
          <p class="about-text">
            <strong>All-In-One Manager</strong> for the Hemp0x blockchain.
          </p>
          <p class="about-text">
            This application controls <strong>hemp0xd</strong> and
            <strong>hemp0x-cli</strong> Core Next binaries built from the official Hemp0x repository.
          </p>
          <p class="about-credit">
            Forked from Ravencoin (December 18, 2025)<br />
            Special thanks to <strong>cc2002cc</strong> and the entire Ravencoin community
            for their foundational work.
          </p>
        </div>

        <div class="about-section disclaimer-section">
          <h3 class="section-header">DISCLAIMER</h3>
          <p class="disclaimer-text">
            This software and all its features are experimental proof-of-concept tools built to demonstrate what is possible on the Hemp0x blockchain.
          </p>
          <p class="disclaimer-text" style="color: #ffcc00; font-weight: 600;">
            Use at your own risk and discretion.
          </p>
          <p class="disclaimer-text">
            By using this application, you acknowledge and agree that:
          </p>
          <ul class="disclaimer-list">
            <li>This software allows you to make <strong>irreversible changes</strong> to your wallet and assets.</li>
            <li>All features are experimental and may contain bugs or unexpected behavior.</li>
            <li>You are <strong>18 years of age or older</strong>.</li>
            <li>You will comply with all applicable local laws and regulations in your jurisdiction.</li>
            <li>The developers, contributors, and maintainers of Hemp0x Commander are <strong>not responsible</strong> for any lost funds, stolen assets, data loss, or any other damages resulting from the use of this software.</li>
          </ul>
          <div class="disclaimer-warnings">
            <p class="disclaimer-warning">Always backup your wallet before using any features.</p>
            <p class="disclaimer-warning">Only download and build Commander from official sources (hemp0x.com or verified GitHub releases).</p>
            <p class="disclaimer-warning">Encrypted wallets require you to remember your password. There is no recovery option.</p>
          </div>
        </div>

        <div class="about-section">
          <h3 class="section-header">CONTRIBUTING</h3>
          <p class="about-text">
            Hemp0x Commander is fully open source. We welcome contributions from developers and the community.
          </p>
        </div>

        <div class="about-section">
          <h3 class="section-header">USEFUL LINKS</h3>
          <div class="about-links">
            <a href="https://hemp0x.com" target="_blank" rel="noopener noreferrer" class="about-link-card">
              <span class="about-link-label">Official Website</span>
              <span class="about-link-url">hemp0x.com</span>
            </a>
            <a href="https://github.com/hemp0x" target="_blank" rel="noopener noreferrer" class="about-link-card">
              <span class="about-link-label">Project Repository</span>
              <span class="about-link-url">github.com/hemp0x</span>
            </a>
          </div>
        </div>

        <div class="about-footer">
          <span class="neon">Hemp0x - We Build Together</span>
        </div>
      </div>
    </div>

    <!-- RAVENCOIN VIEWS -->
    <div class="view-wrapper" class:show={activeTab === "RVN_DASHBOARD"}>
      <ViewRavencoin />
    </div>


    <!-- FOOTER -->
    <div class="app-footer">
      <a href="https://hemp0x.com" target="_blank" class="footer-link"
        >Hemp0x.com</a
      >
    </div>
  </div>

  {#if showWalletPrompt}
    <button
      class="modal-backdrop"
      type="button"
      aria-label="Close wallet prompt"
      on:click={closeWalletPrompt}
    ></button>
    <div class="modal">
      <h3 class="modal-title">
        {walletPromptMode === "encrypt" ? "ENCRYPT WALLET" : "UNLOCK WALLET"}
      </h3>
      <div class="modal-divider"></div>
      {#if walletPromptMode === "encrypt"}
        <p class="modal-text">
          Encryption is permanent. If you lose this password, funds are lost.
        </p>
      {/if}
      {#if walletPromptMode === "encrypt" || walletPromptMode2 === "passphrase" || !walletPinUsable}
        <label class="modal-label" for="wallet-pass">{walletPromptMode === "encrypt" ? "Password" : "Wallet passphrase"}</label>
        <input
          id="wallet-pass"
          type="password"
          class="modal-input"
          bind:value={walletPromptPass}
          placeholder={walletPromptMode === "encrypt" ? "Enter password" : "Enter wallet passphrase"}
          on:keydown={(e) => e.key === "Enter" && confirmWalletPrompt()}
        />
      {/if}
      {#if walletPromptMode === "encrypt"}
        <label class="modal-label" for="wallet-pass-confirm"
          >Confirm Password</label
        >
        <input
          id="wallet-pass-confirm"
          type="password"
          class="modal-input"
          bind:value={walletPromptPassConfirm}
          placeholder="Confirm password"
        />
      {:else}
        {#if walletPromptMode2 === "pin" && walletPinUsable}
          <p class="modal-text">
            Enter this device's 6-digit PIN to unlock the local Core wallet for signing on this device.
          </p>
          <label class="modal-label" for="wallet-pin">Device PIN</label>
          <input
            id="wallet-pin"
            type="password"
            inputmode="numeric"
            pattern="[0-9]*"
            maxlength="6"
            class="modal-input modal-pin-input"
            bind:value={walletPromptPin}
            placeholder="6-digit PIN"
            on:input={(e) => (walletPromptPin = cleanWalletPin(e.currentTarget.value))}
            on:keydown={(e) => e.key === "Enter" && walletPromptPin.length === 6 && confirmWalletPrompt()}
          />
          {#if (walletPinStatus?.lockout_remaining_secs || 0) > 0}
            <p class="modal-text" style="color:#ffaa55;">
              PIN unlock temporarily locked. Try again in {Math.ceil(walletPinStatus.lockout_remaining_secs)}s, or use your wallet passphrase.
            </p>
          {/if}
          <div class="modal-pin-actions">
            <button type="button" class="modal-link-btn" on:click={switchWalletPromptToPassphrase} disabled={walletPromptWorking}>
              Use wallet passphrase
            </button>
            <button type="button" class="modal-link-btn muted" on:click={forgotWalletPromptPin} disabled={walletPromptWorking}>
              Forgot PIN
            </button>
          </div>
        {:else}
          {#if walletPinStatus?.pin_configured && pinRequiresPassphrase(walletPinStatus) && walletPinStatus?.reason}
            <p class="modal-text" style="color:#9fd8af;">
              {walletPinStatus.reason}
            </p>
          {/if}
          <p class="modal-text">
            Unlocking allows Commander to sign transactions for the selected time. The default is 5 minutes. Lock the wallet again when you are done sending.
          </p>
          <label class="modal-label" for="wallet-duration"
            >Duration (seconds)</label
          >
          <input
            id="wallet-duration"
            type="number"
            class="modal-input"
            bind:value={walletPromptDuration}
            min="1"
          />
          {#if walletPinUsable}
            <div class="modal-pin-actions single">
              <button type="button" class="modal-link-btn" on:click={switchWalletPromptToPin} disabled={walletPromptWorking}>
                Use device PIN instead
              </button>
            </div>
          {/if}
        {/if}
      {/if}
      {#if walletPromptError}
        <div class="modal-error" role="alert">{walletPromptError}</div>
      {/if}
      <div class="modal-actions">
        <button class="btn-sm ghost" on:click={closeWalletPrompt}>CANCEL</button
        >
        <button class="btn-sm primary" on:click={confirmWalletPrompt} disabled={walletPromptWorking}>
          {walletPromptMode === "encrypt" ? "ENCRYPT" : (walletPromptMode2 === "pin" && walletPinUsable ? "UNLOCK" : "UNLOCK")}
        </button>
      </div>
    </div>
  {/if}

  <VaultUnlockModal
    show={showVaultUnlockModal}
    bind:password={vaultUnlockPassphrase}
    unlocking={vaultUnlocking}
    error={vaultUnlockError}
    on:cancel={closeVaultUnlockModal}
    on:confirm={confirmVaultUnlock}
  />

  <!-- WELCOME POPUP -->
  {#if showWelcome}
    <div class="welcome-overlay">
      <div class="welcome-modal welcome-modal-disclaimer">
        <div class="welcome-header">
          <h2>IMPORTANT NOTICE</h2>
          <span class="welcome-version">{APP_VERSION}</span>
        </div>
        <div class="welcome-body disclaimer-scroll" bind:this={disclaimerBodyEl} on:scroll={handleDisclaimerScroll}>
          <p class="welcome-text">
            This software and all its features are experimental proof-of-concept tools built to demonstrate what is possible on the Hemp0x blockchain.
          </p>
          <p class="welcome-text" style="color: #ffcc00; font-weight: 600;">
            Use at your own risk and discretion.
          </p>
          <p class="welcome-text" style="margin-bottom: 0.5rem;">
            By continuing, you acknowledge and agree that:
          </p>
          <ul class="disclaimer-list-new">
            <li>This software allows you to make <strong>irreversible changes</strong> to your wallet and assets.</li>
            <li>All features are experimental and may contain bugs or unexpected behavior.</li>
            <li>You are <strong>18 years of age or older</strong>.</li>
            <li>You will comply with all applicable local laws and regulations in your jurisdiction.</li>
            <li>The developers, contributors, and maintainers of Hemp0x Commander are <strong>not responsible</strong> for any lost funds, stolen assets, or any other damages resulting from the use of this software.</li>
          </ul>
          <p class="welcome-text" style="margin-top: 0.75rem; color: #888; font-size: 0.85rem;">
            We strongly recommend reviewing the <strong style="color: var(--color-primary);">ABOUT</strong> page before using any features.
          </p>
        </div>
        <div class="welcome-footer disclaimer-footer">
          <label class="welcome-checkbox">
            <input type="checkbox" bind:checked={showWelcomeOnStartup} />
            <span class="checkmark"></span>
            Show this message on startup
          </label>
          <button class="welcome-btn" class:disabled={!disclaimerScrolled} disabled={!disclaimerScrolled} on:click={closeWelcome}
            >[ Agree + Continue ]</button
          >
        </div>
      </div>
    </div>
  {/if}

  <!-- DAEMON CONFLICT MODAL -->
  {#if showDaemonConflict && conflictRuntimeStatus}
    <div class="welcome-overlay">
      <div class="welcome-modal">
        <div class="welcome-header">
          <h2>Daemon Already Running</h2>
        </div>
        <div class="welcome-body">
          <p class="welcome-text">
            A Hemp0x daemon was detected on the default ports (RPC: {conflictRuntimeStatus.probe.default_rpc_port}, P2P: {conflictRuntimeStatus.probe.default_p2p_port}).
          </p>

          {#if conflictRuntimeStatus.identity}
            {#if conflictRuntimeStatus.identity.rpc_authenticated}
              {#if !conflictRuntimeStatus.processIdentity?.available || !(conflictRuntimeStatus.processIdentity?.sha256_match && conflictRuntimeStatus.processIdentity?.version_commit_match)}
                <p class="welcome-text" style="color: {conflictRuntimeStatus.identity.is_required_core_next ? '#4caf50' : '#ff9800'};">
                  {conflictRuntimeStatus.identity.status}
                </p>
              {/if}
              {#if conflictRuntimeStatus.identity.base_version}
                <p class="welcome-text" style="font-size: 0.85rem; color: #aaa;">
                  Version: {conflictRuntimeStatus.identity.base_version}
                  {#if conflictRuntimeStatus.identity.build}
                    / {conflictRuntimeStatus.identity.build}
                  {:else if conflictRuntimeStatus.identity.subversion}
                    / {conflictRuntimeStatus.identity.subversion}
                  {/if}
                  (Protocol: {conflictRuntimeStatus.identity.protocol_version})
                </p>
              {/if}
              {#if conflictCapabilities?.help_probe_success}
                {#if conflictCapabilityLabels.length > 0}
                  <p class="welcome-text" style="font-size: 0.85rem; color: #4caf50;">
                    Core Next capabilities detected
                  </p>
                  <p class="welcome-text" style="font-size: 0.75rem; color: #888;">
                    {conflictCapabilityLabels.join(", ")}
                  </p>
                {/if}
              {/if}
            {:else}
              <p class="welcome-caution">
                {conflictRuntimeStatus.identity.status}
              </p>
            {/if}
            {#if conflictRuntimeStatus.processIdentity?.available && conflictRuntimeStatus.processIdentity?.sha256_match && conflictRuntimeStatus.processIdentity?.version_commit_match}
              <p class="welcome-text" style="font-size: 0.85rem; color: #4caf50;">
                Process identity: Exact bundled match
              </p>
              <p class="welcome-text" style="font-size: 0.75rem; color: #4caf50;">
                Commit: {conflictRuntimeStatus.required_commit_hash} verified
              </p>
              <p class="welcome-text" style="font-size: 0.75rem; color: #4caf50;">
                Binary hash: Match
              </p>
            {:else if conflictRuntimeStatus.processIdentity?.available && conflictRuntimeStatus.processIdentity?.confidence !== 'none'}
              <p class="welcome-text" style="font-size: 0.85rem; color: #ffaa00;">
                Process identity: {conflictRuntimeStatus.processIdentity.confidence.toUpperCase()} confidence
              </p>
              {#if conflictRuntimeStatus.processIdentity.matches_bundled_path}
                <p class="welcome-text" style="font-size: 0.75rem; color: #888;">
                  Executable: Bundled Core Next
                </p>
              {/if}
              {#if conflictRuntimeStatus.processIdentity.version_commit_match}
                <p class="welcome-text" style="font-size: 0.75rem; color: #4caf50;">
                  Commit: {conflictRuntimeStatus.required_commit_hash} verified
                </p>
              {/if}
              {#if conflictRuntimeStatus.processIdentity.sha256_match}
                <p class="welcome-text" style="font-size: 0.75rem; color: #4caf50;">
                  Binary hash: Match
                </p>
              {/if}
            {/if}
          {:else}
            <p class="welcome-caution">
              A daemon is listening on the default RPC port, but Commander could not verify its version.
            </p>
          {/if}

          {#if !conflictRuntimeStatus.bundled_core_next_ready}
            <p class="welcome-caution">
              The bundled daemon does not match the required Core Next build
              ({conflictRuntimeStatus.required_base_version}-{conflictRuntimeStatus.required_commit_hash}).
              {#if conflictRuntimeStatus.daemon.base_version}
                Bundled binary: v{conflictRuntimeStatus.daemon.base_version}
                {#if conflictRuntimeStatus.daemon.commit_hash}
                  ({conflictRuntimeStatus.daemon.commit_hash})
                {/if}
              {:else}
                Bundled binary: unrecognized version
              {/if}
            </p>
          {/if}
          <p class="welcome-text" style="font-size: 0.85rem; color: #aaa;">
            Choose how to proceed:
          </p>
        </div>
        <div class="welcome-footer" style="flex-wrap: wrap; gap: 0.5rem;">
          <button class="btn-xs" style="flex: 1;" on:click={() => resolveDaemonConflict('continue')}>
            Continue with existing daemon
          </button>
          {#if conflictRuntimeStatus.bundled_core_next_ready}
            <button class="btn-xs" style="flex: 1;" on:click={() => resolveDaemonConflict('stop_and_use_bundled')}>
              Stop it and use bundled Core Next
            </button>
          {/if}
        </div>
      </div>
    </div>
  {/if}

  <!-- CLOSE PROMPT MODAL -->
  {#if showClosePrompt}
    <div class="welcome-overlay">
      <div class="welcome-modal">
        <div class="welcome-header">
          <h2>Daemon Still Running</h2>
        </div>
        <div class="welcome-body">
          <p class="welcome-text">
            Commander is currently managing the hemp0xd daemon. How would you like to proceed?
          </p>
          {#if closePromptStatus}
            <div style="margin-top:0.8rem;">
              <CommanderLoader compact={true} label={closePromptStatus} detail="Please wait while Commander finishes the shutdown request." />
            </div>
          {/if}
        </div>
        <div class="welcome-footer" style="flex-wrap: wrap; gap: 0.5rem;">
          <button class="btn-xs" style="flex: 1;" on:click={closeStopDaemon} disabled={!!closePromptStatus}>
            Stop daemon and exit
          </button>
          <button class="btn-xs" style="flex: 1;" on:click={closeLeaveDaemon} disabled={!!closePromptStatus}>
            Leave daemon running
          </button>
          <button class="btn-xs ghost" style="flex: 1;" on:click={closeCancel} disabled={!!closePromptStatus}>
            Cancel
          </button>
        </div>
      </div>
    </div>
  {/if}

  {#if showRpcAuthHelp}
    <div class="welcome-overlay">
      <div class="welcome-modal rpc-auth-modal">
        <div class="welcome-header">
          <h2>Use Cookie RPC Auth</h2>
        </div>
        <div class="welcome-body">
          <p class="welcome-text">
            Cookie auth is recommended for Commander and Core Next. Core creates a local
            <span class="mono">.cookie</span> file in the active data directory when the daemon starts.
          </p>
          <p class="welcome-text">
            In <span class="mono">hemp.conf</span>, remove or comment active static RPC credential lines:
          </p>
          <pre class="rpc-auth-code">rpcuser=...
rpcpassword=...
rpcauth=...
rpccookiefile=...</pre>
          <p class="welcome-text">
            Lines that start with <span class="mono">#</span> are ignored. Keep
            <span class="mono">server=1</span>, save the config, then restart Core.
          </p>
          <p class="welcome-caution">
            Do not paste RPC passwords into screenshots, logs, or support messages.
          </p>
        </div>
        <div class="welcome-footer" style="flex-wrap: wrap; gap: 0.5rem;">
          <button class="btn-xs" style="flex: 1;" on:click={openRpcAuthConfig}>
            Open Config
          </button>
          <button class="btn-xs ghost" style="flex: 1;" on:click={() => (showRpcAuthHelp = false)}>
            Close
          </button>
        </div>
      </div>
    </div>
  {/if}
</main>

<style lang="css">
  :global(input[type="number"]) {
    color-scheme: dark;
  }
  :global(input[type="number"]::-webkit-inner-spin-button),
  :global(input[type="number"]::-webkit-outer-spin-button) {
    opacity: 1;
    margin-left: 0.22rem;
    cursor: pointer;
  }

  /* --- LAYOUT SHELL --- */
  .shell {
    display: flex;
    flex-direction: column;
    height: 100vh;
    width: 100vw;
    position: relative;
    background: #000; /* PURE VOID */
    /* REMOVED FIXED SCALING - Allow Responsive Layout */
    /* transform: scale(var(--ui-scale, 1)); */
    /* transform-origin: top left; */
    /* width: calc(100% / var(--ui-scale, 1)); */
    /* height: calc(100% / var(--ui-scale, 1)); */
    overflow: hidden;
  }

  /* --- HEADER --- */
  .top-bar {
    min-height: 60px;
    height: auto;
    position: relative;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.5rem 1.25rem;
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
    background: rgba(0, 0, 0, 0.7);
    backdrop-filter: blur(12px);
    z-index: 50;
    -webkit-app-region: no-drag;
    flex-shrink: 0;
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    flex-shrink: 0;
    min-width: 0;
  }
  .logo {
    height: 36px;
    width: 36px;
    border-radius: 8px;
    box-shadow: 0 0 16px rgba(0, 255, 65, 0.2);
  }
  .brand-info {
    min-width: 0;
    overflow: hidden;
  }
  .app-title {
    margin: 0;
    font-size: 1rem;
    letter-spacing: 1.5px;
    color: #fff;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .version {
    font-size: 0.75rem;
    color: var(--color-muted);
    opacity: 0.7;
    text-shadow: none;
  }
  .app-status {
    color: var(--color-primary-dim);
    font-size: 0.65rem;
    letter-spacing: 0.5px;
    margin-top: 2px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  /* Status Traffic Light Dots */
  .status-dot {
    display: inline-block;
    width: 10px;
    height: 10px;
    border-radius: 50%;
    margin-left: 4px;
    opacity: 0.35;
    transition: all 0.3s ease;
    cursor: help;
  }
  .status-dot.green {
    background: #00ff41;
  }
  .status-dot.yellow {
    background: #ffdd00;
  }
  .status-dot.red {
    background: #ff4444;
  }
  .status-dot.active {
    opacity: 1;
  }
  .status-dot.green.active {
    box-shadow: 0 0 4px #00ff41, 0 0 8px rgba(0, 255, 65, 0.5);
  }
  .status-dot.yellow.active {
    box-shadow: 0 0 4px #ffdd00, 0 0 8px rgba(255, 221, 0, 0.5);
  }
  .status-dot.red.active {
    box-shadow: 0 0 4px #ff4444, 0 0 8px rgba(255, 68, 68, 0.5);
  }

  .main-nav {
    display: flex;
    flex-wrap: wrap;
    gap: 0.25rem;
    align-items: center;
    position: relative;
    z-index: 60;
    pointer-events: auto;
    -webkit-app-region: no-drag;
    flex-shrink: 1;
    min-width: 0;
    overflow-x: auto;
    padding-bottom: 2px;
  }
  .main-nav::-webkit-scrollbar {
    height: 2px;
  }
  .main-nav::-webkit-scrollbar-thumb {
    background: rgba(255, 255, 255, 0.2);
    border-radius: 2px;
  }
  .tab-btn {
    pointer-events: auto;
    -webkit-app-region: no-drag;
    white-space: nowrap;
    flex-shrink: 0;
  }

  .window-controls {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
  }

  /* --- TRUST SIGNAL STRIP --- */
  .trust-strip {
    display: flex;
    gap: 0;
    border-bottom: 1px solid rgba(0, 255, 65, 0.08);
    background: rgba(0, 0, 0, 0.5);
    flex-shrink: 0;
    z-index: 40;
    overflow-x: auto;
    min-height: 1.7rem;
  }
  .trust-strip::-webkit-scrollbar {
    height: 0;
  }
  .ts-item {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    padding: 0.3rem 0.75rem;
    border-right: 1px solid rgba(255, 255, 255, 0.04);
    flex-shrink: 0;
    white-space: nowrap;
  }
  button.ts-item {
    background: transparent;
    border-top: none;
    border-left: none;
    border-bottom: none;
    cursor: default;
    font: inherit;
  }
  button.ts-item.wallet-status-action {
    cursor: pointer;
  }
  button.ts-item.wallet-status-action:hover {
    background: rgba(0, 255, 65, 0.05);
    transform: none;
    box-shadow: none;
  }
  button.ts-item:disabled {
    opacity: 1;
  }
  .ts-label {
    font-size: 0.55rem;
    color: #666;
    text-transform: uppercase;
    letter-spacing: 0.5px;
  }
  .ts-val {
    font-size: 0.6rem;
    font-family: var(--font-mono);
    color: #999;
    letter-spacing: 0.5px;
  }
  .ts-ok .ts-val {
    color: var(--color-primary-dim);
  }
  .ts-warn .ts-val {
    color: #ffaa00;
  }
  .ts-bad .ts-val {
    color: #ff5555;
  }
  .stat-state.state-ok {
    color: var(--color-primary);
  }
  .stat-state.state-warn {
    color: #ffaa00;
  }

  /* --- CONTENT CONTAINER --- */
  .content {
    flex: 1;
    position: relative;
    overflow: hidden; /* No Global Scroll */
    padding: 0.8rem 1.5rem 1.5rem 1.5rem;
    display: flex;
    flex-direction: column;
  }
  .content.no-padding {
    padding-bottom: 0;
  }

  /* --- COMMON PANEL STYLE --- */
  .glass-panel {
    background: rgba(5, 7, 6, 0.82);
    backdrop-filter: blur(12px);
    border: 1px solid rgba(0, 255, 65, 0.12);
    border-top: 1px solid rgba(0, 255, 65, 0.18);
    border-bottom: 1px solid rgba(0, 0, 0, 0.55);
    border-radius: 8px;
    box-shadow: 0 12px 30px rgba(0, 0, 0, 0.55);
    padding: 1.1rem;
    display: flex;
    flex-direction: column;
    position: relative;
    overflow: hidden;
  }

  .panel-title {
    margin: 0 0 1.1rem 0;
    font-size: 0.8rem;
    text-transform: uppercase;
    color: rgba(255, 255, 255, 0.85); /* Whiter title */
    letter-spacing: 2px;
    padding-bottom: 0.8rem;
    display: flex;
    justify-content: space-between;
    font-weight: 600;

    /* THE ETCHED EFFECT ENHANCED */
    text-shadow: 0 2px 4px rgba(0, 0, 0, 1);
    border-bottom: 1px solid rgba(255, 255, 255, 0.06);
    box-shadow: 0 1px 0 rgba(0, 0, 0, 0.3);
  }
  .hint {
    font-size: 0.7rem;
    text-transform: none;
    opacity: 0.5;
    font-weight: 400;
  }

  /* --- GLOBAL VIEW TRANSITION --- */
  .fade-in {
    animation: viewFadeIn 0.25s ease-out forwards;
  }
  @keyframes viewFadeIn {
    from {
      opacity: 0;
      transform: translateY(6px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }

  /* --- DASHBOARD GRID --- */
  /* --- DASHBOARD GRID --- */
  .view-dashboard {
    flex: 1; /* GROWS to fill vertical void */
    display: flex;
    flex-direction: column;
    gap: 1.2rem;
    height: 100%; /* Ensure it takes full height */
    min-height: 0;
    overflow: hidden; /* Prevent global scroll */
    padding-right: 4px; /* Space for scrollbar */
    animation: viewFadeIn 0.25s ease-out forwards;
  }

  .row-top {
    flex: 0 0 auto; /* Allow flexible height */
    display: flex;
    gap: 1.2rem;
    /* height: 30vh;  Removed restrictive height */
    /* max-height: 210px; Removed restrictive cap */
    min-height: 130px; /* ULTRA COMPACT (was 160px) */
    transition: height 0.2s;
  }
  .row-bottom {
    flex: 1; /* GROWS to take remaining space */
    display: flex;
    min-height: 0; /* Critical for scrolling */
  }
  .activity-card {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-height: 0; /* Allow shrinking to viewport */
    /* max-height: 390px; REMOVED CAP for growth */
    overflow: hidden; /* Container doesn't overflow */
    transition: all 0.5s cubic-bezier(0.16, 1, 0.3, 1); /* PRO TRANSITION */
  }

  /* --- ACTIVITY EXPANSION OVERLAY --- */
  /* --- ACTIVITY EXPANSION OVERLAY --- */
  .activity-card.expanded {
    position: fixed; /* Break out of grid */
    top: 0; /* Cover EVERYTHING including header */
    left: 0;
    right: 0; /* Use right: 0 instead of width: 100vw to avoid scrollbar overflow */
    bottom: 0;
    z-index: 1500;
    margin: 0.5rem; /* Tiny margin to show rounded corners against edge */
    border-radius: 8px;
    border: none;
    background: rgba(0, 0, 0, 0.9); /* Semi-transparent for blur */
    backdrop-filter: blur(20px); /* GLASS EFFECT */
    padding: 1.5rem; /* Better framing */
    box-sizing: border-box; /* SAFETY: Include padding/border in width */
    border: 1px solid rgba(0, 255, 65, 0.22);
    box-shadow: 0 0 40px rgba(0, 0, 0, 0.8); /* Deep shadow for float effect */
  }
  .activity-card.expanded .panel-header {
    background: transparent;
    border-bottom: 1px solid rgba(0, 255, 65, 0.2);
    padding-right: 2rem; /* FIX CUTOFF TEXT */
  }

  .btn-expand {
    margin-left: 8px;
    color: var(--color-primary);
    font-size: 0.8rem;
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
  .expand-icon {
    font-family: Arial, sans-serif; /* simpler font for arrow */
    font-size: 0.7rem;
  }

  /* History quick-link badge in the RECENT ACTIVITY header */
  .activity-history-btn {
    margin-left: 8px;
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    padding: 0.2rem 0.55rem;
    background: rgba(0, 255, 65, 0.06);
    border: 1px solid rgba(0, 255, 65, 0.2);
    border-bottom: 1px solid rgba(0, 255, 65, 0.2);
    border-radius: 4px;
    color: var(--color-primary);
    font-family: var(--font-mono);
    font-size: 0.55rem;
    font-weight: 600;
    letter-spacing: 0.5px;
    cursor: pointer;
    transition: all 0.15s ease;
    box-shadow: inset 0 0 0 1px rgba(0, 255, 65, 0.06);
    text-transform: uppercase;
  }
  .activity-history-btn:hover {
    background: rgba(0, 255, 65, 0.14);
    border-color: rgba(0, 255, 65, 0.4);
  }
  .activity-history-icon {
    font-size: 0.7rem;
    line-height: 1;
    opacity: 0.85;
  }
  .activity-history-label {
    line-height: 1;
  }
  @media (max-width: 600px) {
    .activity-history-label {
      display: none;
    }
    .activity-history-btn {
      padding: 0.25rem 0.4rem;
    }
  }

  /* --- CYBER PANELS (The Hybrid) --- */
  .cyber-panel {
    flex: 1;
    display: flex;
    flex-direction: column;
    padding: 0;
    border: 1px solid rgba(0, 255, 65, 0.14);
    background: rgba(4, 6, 5, 0.92);
    box-shadow: 0 10px 30px rgba(0, 0, 0, 0.45);
  }

  .wallet-card {
    flex: 1.4;
  }
  .node-card {
    flex: 1;
  }

  /* STATUS LED DOT */
  .stat-pill {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  .stat-pill .value {
    font-size: 0.85rem;
    font-weight: 600;
    letter-spacing: 1px;
    color: var(--color-primary);
  }
  .led {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    transition: all 0.3s ease;
  }
  .led.running {
    background: #00ff41;
    box-shadow: 0 0 6px rgba(0, 255, 65, 0.5);
    animation: pulse-green 2s ease-in-out infinite;
  }
  .led.stopped {
    background: #ff3333;
    box-shadow: 0 0 4px rgba(255, 51, 51, 0.3);
  }
  @keyframes pulse-green {
    0%,
    100% {
      box-shadow: 0 0 6px rgba(0, 255, 65, 0.5);
    }
    50% {
      box-shadow: 0 0 10px rgba(0, 255, 65, 0.7);
    }
  }

  /* PANEL HEADER */
  .panel-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.6rem 1rem;
    background: rgba(0, 0, 0, 0.45);
    border-bottom: 1px solid rgba(0, 255, 65, 0.1);
    flex-shrink: 0;
  }
  .hud-title {
    color: var(--color-muted);
    font-size: 0.75rem;
    letter-spacing: 1px;
    opacity: 0.8;
  }

  /* WINDOW CONTROLS */
  .status-stack {
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 0;
    flex-shrink: 0;
    position: relative;
  }
  .traffic-lights {
    display: flex;
    gap: 6px;
  }

  .network-warning-text {
    font-size: 0.5rem;
    color: #ff3333;
    letter-spacing: 0.5px;
    text-transform: uppercase;
    text-shadow: 0 0 4px rgba(255, 50, 50, 0.4);
    white-space: nowrap;
    margin-top: 2px;
  }

  .network-badge {
    padding: 0.2rem 0.6rem;
    border-radius: 4px;
    font-size: 0.7rem;
    font-weight: bold;
    letter-spacing: 1px;
    margin-right: 1rem;
    border: 1px solid rgba(255, 255, 255, 0.2);
  }
  .network-badge.regtest {
    background: rgba(0, 191, 255, 0.2);
    color: #00bfff;
    border-color: #00bfff;
    box-shadow: 0 0 10px rgba(0, 191, 255, 0.2);
  }

  /* PANEL CONTENT */
  .panel-content {
    flex: 1;
    padding: 0.5rem 1rem; /* ULTRA COMPACT PADDING (was 0.8rem) */
    position: relative;
    display: flex;
    flex-direction: column;
    justify-content: center; /* Center content vertically */
    /* padding-bottom removed, rely on flex gap */
  }

  /* COMPACT WALLET TWEAK */
  .compact-wallet .balance-hero-small .val {
    font-size: 2.5rem; /* Slightly smaller if needed, but flex gap handles it */
    margin-bottom: -4px; /* Tighten line height gap */
  }

  /* PERSISTENT VIEWS */
  .view-wrapper {
    display: none;
    flex: 1;
    flex-direction: column;
    height: 100%;
    width: 100%;
    min-height: 0; /* Crucial for nested scroll */
    animation: fade-in 0.2s ease-out;
    will-change: transform, opacity;
  }
  .view-wrapper.show {
    display: flex;
  }
  .wallet-content {
    justify-content: center;
    align-items: center;
    text-align: center;
  }

  /* PANEL ACTIONS */
  .panel-actions {
    padding: 0.8rem 1.2rem;
    background: rgba(0, 0, 0, 0.35);
    border-top: 1px solid rgba(0, 255, 65, 0.08);
    display: flex;
    gap: 0.8rem;
    align-items: center;
    min-height: 3.1rem;
    flex-wrap: nowrap;
  }
  .panel-actions.right {
    justify-content: flex-end;
  }

  .daemon-status-text {
    font-size: 0.65rem;
    color: var(--color-primary);
    letter-spacing: 0.5px;
    flex: 1 1 auto;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .daemon-poll-text {
    font-size: 0.6rem;
    color: #888;
    letter-spacing: 0.3px;
    flex: 0 1 auto;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .daemon-loader {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    min-width: 0;
    flex: 0 1 auto;
  }
  .daemon-loader-label {
    font-size: 0.6rem;
    color: var(--color-primary);
    letter-spacing: 0.5px;
    font-family: var(--font-mono);
  }

  /* SMALL ACTION BUTTONS */
  .btn-xs {
    padding: 0.5rem 1rem;
    font-size: 0.75rem;
    font-weight: 600;
    letter-spacing: 1px;
    text-transform: uppercase;
    background: rgba(0, 255, 65, 0.15);
    border: 1px solid var(--color-primary);
    border-bottom: 1px solid var(--color-primary);
    color: var(--color-primary);
    border-radius: 4px;
    cursor: pointer;
    transition: all 0.2s ease;
    white-space: nowrap;
    flex-shrink: 0;
    box-shadow: inset 0 0 0 1px rgba(0, 255, 65, 0.16);
  }
  .btn-xs:hover {
    background: rgba(0, 255, 65, 0.25);
    box-shadow: 0 0 10px rgba(0, 255, 65, 0.22);
  }
  .btn-xs.ghost {
    background: transparent;
    border-color: rgba(255, 255, 255, 0.2);
    color: var(--color-muted);
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.08);
  }
  .btn-xs.ghost:hover {
    border-color: #ff4444;
    color: #ff4444;
    box-shadow: 0 0 8px rgba(255, 68, 68, 0.3);
  }
  .btn-xs.disabled,
  .btn-xs:disabled {
    background: rgba(100, 100, 100, 0.2);
    border-color: #555;
    color: #666;
    cursor: not-allowed;
    box-shadow: none;
  }
  .btn-xs.disabled:hover,
  .btn-xs:disabled:hover {
    background: rgba(100, 100, 100, 0.2);
    box-shadow: none;
  }

  /* --- NODE CARD STATS --- */
  .stat-grid-compact {
    display: flex;
    flex-direction: column;
    gap: 0.5rem; /* TIGHT STATS GAP (was 1rem) */
    justify-content: center;
    height: 100%;
  }
  .stat-pair {
    display: flex;
    justify-content: space-between;
    align-items: center;
    border-bottom: 1px solid rgba(255, 255, 255, 0.03);
    padding-bottom: 0.5rem;
  }
  .stat-pair .label {
    font-size: 0.7rem;
    color: var(--color-muted);
    letter-spacing: 1px;
  }
  .stat-pair .mono {
    font-size: 1rem;
    color: #fff;
  }
  .repair-progress {
    margin-top: 0.55rem;
    padding: 0.5rem 0.6rem;
    border: 1px solid rgba(255, 170, 0, 0.24);
    border-radius: 5px;
    background: rgba(255, 170, 0, 0.04);
  }
  .repair-progress-head,
  .repair-progress-detail {
    display: flex;
    justify-content: space-between;
    gap: 0.75rem;
    color: #d7a64a;
    font-family: var(--font-mono);
    font-size: 0.62rem;
    line-height: 1.35;
  }
  .repair-progress-track {
    height: 4px;
    margin: 0.42rem 0;
    overflow: hidden;
    border-radius: 2px;
    background: rgba(255, 255, 255, 0.08);
  }
  .repair-progress-track span {
    display: block;
    height: 100%;
    min-width: 2px;
    border-radius: inherit;
    background: #ffaa00;
    box-shadow: 0 0 8px rgba(255, 170, 0, 0.4);
    transition: width 0.35s ease;
  }
  .repair-progress-track span.indeterminate {
    width: 35%;
    animation: repair-progress-slide 1.4s ease-in-out infinite;
  }
  @keyframes repair-progress-slide {
    from { transform: translateX(-110%); }
    to { transform: translateX(300%); }
  }

  /* --- WALLET HERO SMALL --- */
  .balance-hero-small .val {
    font-size: 1.8rem; /* ULTRA COMPACT (was 2.5rem) */
    font-weight: 700;
    font-family: var(--font-mono);
    color: #fff;
    line-height: 1;
    margin-bottom: -2px; /* Tighten Gap */
  }
  .balance-hero-small .unit {
    font-size: 1rem;
    color: var(--color-primary);
  }
  .balance-hero-small .sub {
    font-size: 0.65rem;
    color: var(--color-muted);
    letter-spacing: 2px;
    margin-top: 0.2rem;
  }
  .wallet-metrics {
    display: flex;
    flex-wrap: wrap;
    gap: 1rem;
    font-size: 0.65rem;
    color: var(--color-muted);
    margin-top: 0.75rem;
    opacity: 0.7;
    justify-content: center;
  }
  .wallet-metrics .metric {
    font-family: var(--font-mono);
    letter-spacing: 0.5px;
  }

  /* --- ACTIVITY LIST --- */
  .header-row {
    display: grid;
    grid-template-columns: 120px 100px 150px 80px 1fr;
    padding: 0.6rem 1.2rem;
    border-bottom: 1px solid rgba(0, 255, 65, 0.15);
    color: var(--color-muted);
    font-size: 0.7rem;
    text-transform: uppercase;
    font-weight: 600;
    background: rgba(0, 255, 65, 0.02);
  }
  .scroll-body {
    flex: 1;
    overflow-y: scroll; /* Force track visibility */
    padding: 0;
    min-height: 0;
    /* Optional: Right border to separate scrollbar area */
    border-right: 1px solid rgba(255, 255, 255, 0.02);
  }

  /* WebKit Scrollbar (Chrome/Edge/WebView2) */
  .scroll-body::-webkit-scrollbar {
    width: 8px; /* Slightly wider for visibility */
    height: 8px; /* Horizontal scrollbar if needed */
  }
  .scroll-body::-webkit-scrollbar-track {
    background: rgba(0, 255, 65, 0.06); /* VISIBLE TRACK (Faint Green) */
    border-left: 1px solid rgba(0, 255, 65, 0.1); /* Separator line */
  }
  .scroll-body::-webkit-scrollbar-thumb {
    background: rgba(0, 255, 65, 0.3);
    border: 1px solid rgba(0, 255, 65, 0.1);
    border-radius: 0; /* Boxy tech look */
  }
  .scroll-body::-webkit-scrollbar-thumb:hover {
    background: rgba(0, 255, 65, 0.6);
    box-shadow: 0 0 10px rgba(0, 255, 65, 0.4);
  }
  .data-row {
    display: grid;
    grid-template-columns: 120px 100px 150px 80px 1fr;
    padding: 0.8rem 1.2rem;
    border-bottom: 1px solid rgba(255, 255, 255, 0.03);
    font-size: 0.9rem;
    align-items: center;
    transition: background 0.2s;
  }
  .data-row:hover {
    background: rgba(0, 255, 65, 0.05);
  }
  .recent-tx-actions {
    display: flex;
    min-width: 0;
    align-items: center;
    gap: 0.3rem;
  }
  .recent-tx-actions .txid {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .activity-icon-btn {
    display: inline-grid;
    flex: 0 0 auto;
    width: 1.6rem;
    height: 1.6rem;
    place-items: center;
    padding: 0;
    border: 1px solid rgba(0, 255, 65, 0.16);
    border-radius: 4px;
    background: rgba(0, 255, 65, 0.03);
    color: #888;
    font-size: 0.7rem;
    letter-spacing: 0;
  }
  .activity-icon-btn:hover {
    border-color: rgba(0, 255, 65, 0.45);
    background: rgba(0, 255, 65, 0.08);
    color: var(--color-primary);
    transform: none;
  }

  .status-chip {
    font-size: 0.7rem;
    font-weight: bold;
    padding: 0.2rem 0.6rem;
    border-radius: 4px;
    border: 1px solid var(--color-primary);
    color: var(--color-primary);
    transition: all 0.3s ease;
    background: transparent;
    cursor: pointer;
    font-family: var(--font-mono);
  }
  .status-chip:disabled {
    cursor: default;
    opacity: 0.7;
  }
  .status-chip:hover:not(:disabled) {
    transform: translateY(-1px);
  }
  .status-chip.status-red {
    color: #ff4444;
    border-color: rgba(255, 68, 68, 0.5);
    background: rgba(255, 68, 68, 0.1);
    box-shadow: 0 0 10px rgba(255, 68, 68, 0.2);
  }
  .status-chip.status-green {
    color: #00ff41;
    border-color: rgba(0, 255, 65, 0.5);
    background: rgba(0, 255, 65, 0.1);
    box-shadow: 0 0 10px rgba(0, 255, 65, 0.2);
  }
  .status-chip.status-warn {
    color: #ffaa00;
    border-color: rgba(255, 170, 0, 0.5);
    background: rgba(255, 170, 0, 0.1);
    box-shadow: 0 0 10px rgba(255, 170, 0, 0.15);
  }

  .amount.pos {
    color: var(--color-primary);
  }
  .amount.neg {
    color: #ff8888;
  }

  .dim {
    color: var(--color-muted);
    font-size: 0.9rem;
  }
  .txid {
    color: #888;
    font-size: 0.85rem;
    font-family: var(--font-mono);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .error-line {
    margin-top: 0.5rem;
    color: var(--color-danger);
    font-size: 0.75rem;
    opacity: 0.85;
  }

  /* --- FOOTER LINK --- */
  .app-footer {
    text-align: center;
    padding: 0.5rem 0;
    margin-top: auto;
    z-index: 20;
    flex-shrink: 0;
  }

  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.82);
    backdrop-filter: blur(3px);
    z-index: 20000;
    border: none;
    padding: 0;
    cursor: pointer;
  }
  .modal {
    position: fixed;
    top: 50%;
    left: 50%;
    transform: translate(-50%, -50%);
    width: min(400px, 90vw);
    background: rgba(2, 4, 3, 0.98);
    border: 1px solid rgba(0, 255, 65, 0.22);
    border-radius: 8px;
    padding: 1.5rem;
    box-shadow: 0 20px 50px rgba(0, 0, 0, 0.8);
    z-index: 20001;
  }
  .modal-title {
    margin: 0;
    color: var(--color-primary);
    font-family: var(--font-mono);
    font-size: 0.9rem;
    letter-spacing: 1px;
    text-align: center;
  }
  .modal-divider {
    height: 1px;
    margin: 0.75rem 0;
    background: linear-gradient(90deg, transparent, rgba(0, 255, 65, 0.4), transparent);
  }
  .modal-text {
    margin: 0 0 0.75rem 0;
    color: #aaa;
    font-size: 0.7rem;
    line-height: 1.45;
  }
  .modal-label {
    display: block;
    margin: 0.5rem 0 0.35rem 0;
    color: #888;
    font-size: 0.62rem;
    text-transform: uppercase;
    letter-spacing: 1px;
  }
  .modal-input {
    width: 100%;
    background: rgba(0, 0, 0, 0.62);
    border: 1px solid rgba(0, 255, 65, 0.24);
    color: #fff;
    font-family: var(--font-mono);
    font-size: 0.72rem;
    padding: 0.55rem 0.65rem;
    border-radius: 6px;
    outline: none;
  }
  .modal-input:focus {
    border-color: var(--color-primary);
    box-shadow: 0 0 12px rgba(0, 255, 65, 0.15);
  }
  .modal-pin-input {
    text-align: center;
    letter-spacing: 0.45rem;
    font-size: 1rem;
    color: var(--color-primary);
    background: rgba(0, 0, 0, 0.72);
    border-width: 1.5px;
  }
  .modal-pin-actions {
    display: flex;
    justify-content: space-between;
    gap: 0.5rem;
    margin-top: 0.45rem;
    flex-wrap: wrap;
  }
  .modal-pin-actions.single {
    justify-content: flex-start;
  }
  .modal-link-btn {
    background: none;
    border: none;
    color: #7fb69b;
    font-family: var(--font-mono);
    font-size: 0.66rem;
    letter-spacing: 0.3px;
    cursor: pointer;
    text-decoration: underline;
    text-underline-offset: 2px;
    padding: 0;
  }
  .modal-link-btn:hover:not(:disabled) {
    color: var(--color-primary);
  }
  .modal-link-btn.muted {
    color: #9a8a8a;
  }
  .modal-link-btn.muted:hover:not(:disabled) {
    color: #ff7777;
  }
  .modal-link-btn:disabled {
    cursor: not-allowed;
    opacity: 0.5;
    text-decoration: none;
  }
  .modal-error {
    margin-top: 0.75rem;
    border: 1px solid rgba(255, 85, 85, 0.28);
    border-radius: 5px;
    background: rgba(255, 85, 85, 0.09);
    color: #ff7777;
    font-size: 0.64rem;
    padding: 0.5rem 0.6rem;
  }
  .modal-actions {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 0.5rem;
    margin-top: 1rem;
  }
  .modal-actions .btn-sm {
    min-height: 2.35rem;
    border-radius: 5px;
    font-family: var(--font-mono);
    font-size: 0.68rem;
    font-weight: 700;
    letter-spacing: 0.8px;
  }
  .btn-sm.ghost {
    background: transparent;
    color: var(--color-muted);
    border: 1px solid rgba(255, 255, 255, 0.18);
    border-bottom: 1px solid rgba(255, 255, 255, 0.18);
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.04);
  }
  .btn-sm.primary {
    border: 1px solid var(--color-primary);
    border-bottom: 1px solid var(--color-primary);
    background: rgba(0, 255, 65, 0.12);
    color: var(--color-primary);
    box-shadow: inset 0 0 0 1px rgba(0, 255, 65, 0.16);
  }
  .footer-link {
    font-family: var(--font-mono);
    color: var(--color-primary-dim);
    font-size: 0.75rem;
    text-decoration: none;
    letter-spacing: 1px;
    opacity: 0.6;
    transition: all 0.2s;
  }
  .footer-link:hover {
    color: var(--color-primary);
    opacity: 1;
  }

  /* --- WELCOME POPUP --- */
  .welcome-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.85);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 30000;
    animation: welcomeFadeIn 0.3s ease-out;
  }
  @keyframes welcomeFadeIn {
    from {
      opacity: 0;
    }
    to {
      opacity: 1;
    }
  }
  .welcome-modal {
    background: rgba(2, 4, 3, 0.98);
    border: 1px solid rgba(0, 255, 65, 0.2);
    border-radius: 8px;
    box-shadow: 0 20px 50px rgba(0, 0, 0, 0.8);
    max-width: 480px;
    width: 90%;
    animation: slideUp 0.3s ease-out;
  }
  .rpc-auth-modal {
    max-width: 560px;
  }
  .rpc-auth-code {
    margin: 0.75rem 0;
    padding: 0.75rem 0.9rem;
    border: 1px solid rgba(0, 255, 65, 0.16);
    border-radius: 8px;
    background: rgba(0, 0, 0, 0.45);
    color: #d7fbe1;
    font-family: var(--font-mono);
    font-size: 0.78rem;
    line-height: 1.6;
    white-space: pre-wrap;
  }
  @keyframes slideUp {
    from {
      opacity: 0;
      transform: translateY(20px);
    }
    to {
      opacity: 1;
      transform: translateY(0);
    }
  }
  .welcome-header {
    padding: 1.5rem 1.5rem 1rem;
    border-bottom: 1px solid rgba(0, 255, 65, 0.1);
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .welcome-header h2 {
    margin: 0;
    color: var(--color-primary);
    font-size: 1.1rem;
    font-weight: 600;
    letter-spacing: 1px;
  }
  .welcome-version {
    color: #555;
    font-family: var(--font-mono);
    font-size: 0.8rem;
  }
  .welcome-body {
    padding: 1.5rem;
  }
  .welcome-text {
    color: #ccc;
    font-size: 0.95rem;
    margin: 0 0 1rem 0;
    line-height: 1.5;
  }
  .welcome-text strong {
    color: #ff6b6b;
  }
  .welcome-caution {
    color: #ffcc00;
    font-size: 0.9rem;
    margin: 0 0 1rem 0;
    padding: 0.8rem;
    background: rgba(255, 204, 0, 0.08);
    border: 1px solid rgba(255, 204, 0, 0.2);
    border-radius: 6px;
  }
  .welcome-disclaimer {
    color: #888;
    font-size: 0.85rem;
    margin: 0;
    font-style: italic;
  }
  .disclaimer-list-new {
    margin: 0;
    padding-left: 1.2rem;
    color: #bbb;
    font-size: 0.85rem;
    line-height: 1.7;
  }
  .disclaimer-list-new li {
    margin-bottom: 0.3rem;
  }
  .disclaimer-list-new strong {
    color: #ff6b6b;
  }
  .welcome-footer {
    padding: 1rem 1.5rem;
    border-top: 1px solid rgba(255, 255, 255, 0.05);
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .welcome-checkbox {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    color: #888;
    font-size: 0.8rem;
    cursor: pointer;
  }
  .welcome-checkbox input {
    width: 16px;
    height: 16px;
    accent-color: var(--color-primary);
  }
  .welcome-btn {
    background: var(--color-primary);
    border: none;
    color: #000;
    font-family: var(--font-mono);
    font-size: 0.78rem;
    font-weight: 700;
    padding: 0.8rem 1.2rem;
    border-radius: 6px;
    cursor: pointer;
    letter-spacing: 1px;
    transition: all 0.2s;
    box-shadow: 0 0 16px rgba(0, 255, 65, 0.2);
    white-space: nowrap;
    flex-shrink: 0;
  }
  .welcome-btn:hover {
    box-shadow: 0 0 14px rgba(0, 255, 65, 0.28);
  }
  .welcome-btn.disabled {
    background: #444;
    color: #777;
    cursor: not-allowed;
    box-shadow: none;
  }
  .welcome-btn.disabled:hover {
    box-shadow: none;
  }
  .welcome-modal-disclaimer {
    max-height: 85vh;
    display: flex;
    flex-direction: column;
  }
  .disclaimer-scroll {
    flex: 1 1 0%;
    overflow-y: auto;
    min-height: 0;
    scrollbar-width: thin;
    scrollbar-color: rgba(0, 255, 65, 0.3) transparent;
  }
  .disclaimer-scroll::-webkit-scrollbar { width: 5px; }
  .disclaimer-scroll::-webkit-scrollbar-track { background: transparent; }
  .disclaimer-scroll::-webkit-scrollbar-thumb { background: rgba(0, 255, 65, 0.3); border-radius: 3px; }
  .disclaimer-footer {
    flex-shrink: 0;
  }

  /* --- ABOUT PAGE --- */
  .about-page {
    flex: 1;
    height: 100%;
    display: flex;
    flex-direction: column;
    overflow-y: auto;
    padding: 1.5rem 2rem;
    padding-bottom: 5rem;
    margin: 4px;
    min-height: 0;
    scrollbar-width: thin;
    scrollbar-color: rgba(0, 255, 65, 0.3) transparent;
  }
  .about-page::-webkit-scrollbar { width: 6px; }
  .about-page::-webkit-scrollbar-track { background: transparent; }
  .about-page::-webkit-scrollbar-thumb { background: rgba(0, 255, 65, 0.3); border-radius: 3px; }
  .about-header {
    display: flex;
    align-items: center;
    gap: 1.5rem;
    padding-bottom: 1.5rem;
    border-bottom: 1px solid rgba(0, 255, 65, 0.1);
    margin-bottom: 1.5rem;
  }
  .about-logo {
    width: 80px;
    height: 80px;
    border-radius: 8px;
    box-shadow: 0 0 20px rgba(0, 255, 65, 0.15);
    object-fit: contain;
  }
  .about-title-block {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }
  .about-title {
    margin: 0;
    color: white;
    font-size: 1.3rem;
    letter-spacing: 3px;
  }
  .about-version {
    color: var(--color-primary);
    font-family: var(--font-mono);
    font-size: 0.85rem;
  }
  .welcome-toggle {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    color: #888;
    font-size: 0.75rem;
    cursor: pointer;
  }
  .welcome-toggle input {
    width: 14px;
    height: 14px;
    accent-color: var(--color-primary);
  }
  .welcome-toggle:hover {
    color: #aaa;
  }
  .about-section {
    margin-bottom: 1.5rem;
  }
  .section-header {
    color: var(--color-primary);
    font-size: 0.75rem;
    margin: 0 0 0.8rem 0;
    letter-spacing: 2px;
    text-transform: uppercase;
    font-weight: 700;
    padding-bottom: 0.5rem;
    border-bottom: 1px solid rgba(255, 255, 255, 0.05);
  }
  .about-text {
    color: #bbb;
    font-size: 0.88rem;
    line-height: 1.6;
    margin: 0 0 0.6rem 0;
  }
  .about-text strong {
    color: #fff;
  }
  .about-credit {
    color: #888;
    font-size: 0.82rem;
    line-height: 1.6;
    margin: 0.5rem 0 0 0;
    padding-top: 0.5rem;
    border-top: 1px solid rgba(255, 255, 255, 0.04);
  }
  .about-credit strong {
    color: var(--color-primary);
  }
  .disclaimer-section {
    background: rgba(255, 100, 100, 0.03);
    border: 1px solid rgba(255, 100, 100, 0.08);
    border-radius: 8px;
    padding: 1rem 1.2rem;
  }
  .disclaimer-section .section-header {
    color: #ff8888;
    border-bottom-color: rgba(255, 100, 100, 0.1);
  }
  .disclaimer-text {
    color: #ccc;
    font-size: 0.85rem;
    line-height: 1.6;
    margin: 0 0 0.6rem 0;
  }
  .disclaimer-list {
    margin: 0;
    padding-left: 1.5rem;
    color: #aaa;
    font-size: 0.85rem;
    line-height: 1.9;
  }
  .disclaimer-list li {
    margin-bottom: 0.2rem;
  }
  .disclaimer-list strong {
    color: #ff8888;
  }
  .disclaimer-warnings {
    margin-top: 0.8rem;
    padding-top: 0.6rem;
    border-top: 1px solid rgba(255, 100, 100, 0.08);
  }
  .disclaimer-warning {
    margin: 0 0 0.4rem 0;
    font-size: 0.78rem;
    color: #ffcc00;
    line-height: 1.5;
    padding-left: 1rem;
    position: relative;
  }
  .disclaimer-warning::before {
    content: "!";
    position: absolute;
    left: 0;
    color: #ffcc00;
    font-weight: 700;
  }
  .about-links {
    display: flex;
    gap: 0.8rem;
    flex-wrap: wrap;
  }
  .about-link-card {
    display: flex;
    flex-direction: column;
    gap: 0.2rem;
    padding: 0.6rem 1rem;
    background: rgba(0, 255, 65, 0.03);
    border: 1px solid rgba(0, 255, 65, 0.12);
    border-radius: 6px;
    text-decoration: none;
    transition: all 0.2s;
    flex: 1;
    min-width: 160px;
  }
  .about-link-card:hover {
    background: rgba(0, 255, 65, 0.06);
    border-color: rgba(0, 255, 65, 0.25);
    box-shadow: 0 0 12px rgba(0, 255, 65, 0.1);
  }
  .about-link-label {
    font-size: 0.7rem;
    color: #888;
    letter-spacing: 0.5px;
    text-transform: uppercase;
  }
  .about-link-url {
    font-size: 0.85rem;
    color: var(--color-primary);
    font-family: var(--font-mono);
    letter-spacing: 0.5px;
  }
  .about-footer {
    margin-top: auto;
    padding-top: 1.5rem;
    border-top: 1px solid rgba(255, 255, 255, 0.05);
    display: flex;
    justify-content: center;
    align-items: center;
  }
  /* --- HEADER RESPONSIVENESS --- */

  @media (max-width: 900px) {
    .top-bar {
      padding: 0 0.75rem;
    }
    .app-title {
      font-size: 0.85rem;
      letter-spacing: 1px;
    }
    .app-status {
      display: none;
    }
    .tab-btn {
      padding: 0.85rem 0.6rem;
      font-size: 0.7rem;
    }
    .content {
      padding: 0.6rem 0.75rem 0.75rem 0.75rem;
    }
    .ts-item {
      padding: 0.2rem 0.5rem;
    }
  }

  @media (max-width: 800px) {
    .top-bar {
      min-height: 52px;
      height: auto;
    }
    .app-title {
      font-size: 0.8rem;
    }
    .version {
      display: none;
    }
    .brand-info {
      display: flex;
      flex-direction: column;
      justify-content: center;
    }
    .tab-btn {
      padding: 0 0.5rem;
      font-size: 0.65rem;
    }
    .main-nav {
      gap: 0;
    }
    .ts-item {
      padding: 0.15rem 0.4rem;
    }
    .ts-label {
      font-size: 0.5rem;
    }
    .ts-val {
      font-size: 0.55rem;
    }
    .header-row,
    .data-row {
      grid-template-columns: 90px 80px 120px 60px 1fr;
      font-size: 0.8rem;
      padding: 0.5rem 0.8rem;
    }
    .row-top {
      flex-direction: column;
      gap: 0.8rem;
      min-height: auto;
    }
    .node-card,
    .wallet-card {
      flex: none;
    }
  }

  @media (max-width: 600px) {
    .top-bar {
      min-height: 48px;
      height: auto;
      padding: 0 0.5rem;
    }
    .logo {
      height: 28px;
      width: 28px;
    }
    .brand-info {
      gap: 0;
    }
    .tab-btn {
      padding: 0 0.4rem;
      font-size: 0.6rem;
      letter-spacing: 0;
    }
    .header-row,
    .data-row {
      grid-template-columns: 70px 70px 100px 50px 1fr;
      font-size: 0.7rem;
      padding: 0.4rem 0.6rem;
    }
  }

  @media (max-height: 700px) {
    .top-bar {
      min-height: 52px;
      height: auto;
      padding: 0 1rem;
    }
    .trust-strip {
      min-height: 22px;
    }
    .ts-item {
      padding: 0.18rem 0.55rem;
      font-size: 0.6rem;
    }
    .row-top {
      min-height: 100px;
    }
    .panel-header {
      padding: 0.5rem 0.8rem;
    }
    .panel-content {
      padding: 0.3rem 0.8rem;
    }
    .stat-pair {
      padding-bottom: 0.3rem;
    }
    .balance-hero-small .val {
      font-size: 1.5rem;
    }
  }
</style>

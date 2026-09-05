/**
 * The UDS requests the response editor offers, service by service and sub-function by
 * sub-function.
 *
 * Two things this list is careful about. Sub-function services are listed with their actual
 * sub-functions, because `19 02` and `19 04` are different requests with different parameter
 * shapes — offering only one of them is what made the editor feel limited. And services the
 * engine's UDS plugin does not implement are marked, because for those an override is the only
 * way to get a positive response at all.
 *
 * Byte templates are starting points, not gospel: every field stays editable before the
 * override is added, and the engine validates what is finally sent.
 */

export interface CatalogueVariant {
  /** What this variant is, e.g. 'reportDTCByStatusMask (0x02)'. */
  label: string
  requestHex: string
  responseHex: string
  /** Runs of the request the response should echo, for a wildcard template. */
  echoSpans?: { requestOffset: number; length: number; responseOffset: number }[]
}

export interface CatalogueService {
  /** Service id as hex, e.g. '19'. */
  sid: string
  name: string
  /** False when only an override can produce a positive response for this service. */
  implemented: boolean
  variants: CatalogueVariant[]
}

/** Services the bundled UDS plugin answers on its own. */
export const IMPLEMENTED_SERVICES = ['10', '11', '19', '22', '27', '31', '3E']

/** Echo the two identifier bytes of a `22`/`2E`-shaped request into the response. */
const c_echoIdentifier = [{ requestOffset: 1, length: 2, responseOffset: 1 }]

export const UDS_CATALOGUE: CatalogueService[] = [
  {
    sid: '10',
    name: 'DiagnosticSessionControl',
    implemented: true,
    variants: [
      { label: 'defaultSession (0x01)', requestHex: '10 01', responseHex: '50 01 00 32 01 F4' },
      { label: 'programmingSession (0x02)', requestHex: '10 02', responseHex: '50 02 00 32 01 F4' },
      { label: 'extendedSession (0x03)', requestHex: '10 03', responseHex: '50 03 00 32 01 F4' },
      { label: 'safetySystemSession (0x04)', requestHex: '10 04', responseHex: '50 04 00 32 01 F4' },
    ],
  },
  {
    sid: '11',
    name: 'ECUReset',
    implemented: true,
    variants: [
      { label: 'hardReset (0x01)', requestHex: '11 01', responseHex: '51 01' },
      { label: 'keyOffOnReset (0x02)', requestHex: '11 02', responseHex: '51 02' },
      { label: 'softReset (0x03)', requestHex: '11 03', responseHex: '51 03' },
      {
        label: 'enableRapidPowerShutDown (0x04)',
        requestHex: '11 04',
        responseHex: '51 04 0A',
      },
      { label: 'disableRapidPowerShutDown (0x05)', requestHex: '11 05', responseHex: '51 05' },
    ],
  },
  {
    sid: '14',
    name: 'ClearDiagnosticInformation',
    implemented: false,
    variants: [
      { label: 'all groups (FF FF FF)', requestHex: '14 FF FF FF', responseHex: '54' },
      { label: 'powertrain group', requestHex: '14 00 00 00', responseHex: '54' },
      { label: 'any group', requestHex: '14 ** ** **', responseHex: '54' },
    ],
  },
  {
    sid: '19',
    name: 'ReadDTCInformation',
    implemented: true,
    variants: [
      {
        label: 'reportNumberOfDTCByStatusMask (0x01)',
        requestHex: '19 01 FF',
        responseHex: '59 01 FF 01 00 00',
      },
      { label: 'reportDTCByStatusMask (0x02)', requestHex: '19 02 FF', responseHex: '59 02 FF' },
      {
        label: 'reportDTCSnapshotRecordByDTCNumber (0x04)',
        requestHex: '19 04 12 34 56 01',
        responseHex: '59 04 12 34 56 2F 01 00',
      },
      {
        label: 'reportDTCExtDataRecordByDTCNumber (0x06)',
        requestHex: '19 06 12 34 56 FF',
        responseHex: '59 06 12 34 56 2F 01 00',
      },
      { label: 'reportSupportedDTC (0x0A)', requestHex: '19 0A', responseHex: '59 0A FF' },
      {
        label: 'reportDTCFaultDetectionCounter (0x14)',
        requestHex: '19 14',
        responseHex: '59 14',
      },
      {
        label: 'reportDTCWithPermanentStatus (0x15)',
        requestHex: '19 15',
        responseHex: '59 15 FF',
      },
      { label: 'any sub-function', requestHex: '19 **', responseHex: '59 00' },
    ],
  },
  {
    sid: '22',
    name: 'ReadDataByIdentifier',
    implemented: true,
    variants: [
      { label: 'VIN (0xF190)', requestHex: '22 F1 90', responseHex: '62 F1 90 00' },
      { label: 'ECU serial number (0xF18C)', requestHex: '22 F1 8C', responseHex: '62 F1 8C 00' },
      {
        label: 'ECU hardware number (0xF191)',
        requestHex: '22 F1 91',
        responseHex: '62 F1 91 00',
      },
      {
        label: 'ECU software number (0xF194)',
        requestHex: '22 F1 94',
        responseHex: '62 F1 94 00',
      },
      {
        label: 'active diagnostic session (0xF186)',
        requestHex: '22 F1 86',
        responseHex: '62 F1 86 01',
      },
      { label: 'OEM identifier (0xFD00)', requestHex: '22 FD 00', responseHex: '62 FD 00 00' },
      {
        label: 'any identifier',
        requestHex: '22 ** **',
        responseHex: '62 00 00 00',
        echoSpans: c_echoIdentifier,
      },
    ],
  },
  {
    sid: '23',
    name: 'ReadMemoryByAddress',
    implemented: false,
    variants: [
      {
        label: '2-byte address, 1-byte size',
        requestHex: '23 12 20 00 04',
        responseHex: '63 00 00 00 00',
      },
      {
        label: '4-byte address, 2-byte size',
        requestHex: '23 24 20 00 00 00 00 04',
        responseHex: '63 00 00 00 00',
      },
    ],
  },
  {
    sid: '27',
    name: 'SecurityAccess',
    implemented: true,
    variants: [
      { label: 'requestSeed, level 1 (0x01)', requestHex: '27 01', responseHex: '67 01 11 22 33 44' },
      { label: 'sendKey, level 1 (0x02)', requestHex: '27 02 ** ** ** **', responseHex: '67 02' },
      { label: 'requestSeed, level 2 (0x03)', requestHex: '27 03', responseHex: '67 03 55 66 77 88' },
      { label: 'sendKey, level 2 (0x04)', requestHex: '27 04 ** ** ** **', responseHex: '67 04' },
    ],
  },
  {
    sid: '28',
    name: 'CommunicationControl',
    implemented: false,
    variants: [
      { label: 'enableRxAndTx (0x00)', requestHex: '28 00 01', responseHex: '68 00' },
      { label: 'enableRxAndDisableTx (0x01)', requestHex: '28 01 01', responseHex: '68 01' },
      { label: 'disableRxAndEnableTx (0x02)', requestHex: '28 02 01', responseHex: '68 02' },
      { label: 'disableRxAndTx (0x03)', requestHex: '28 03 01', responseHex: '68 03' },
    ],
  },
  {
    sid: '2E',
    name: 'WriteDataByIdentifier',
    implemented: false,
    variants: [
      { label: 'VIN (0xF190)', requestHex: '2E F1 90 00', responseHex: '6E F1 90' },
      { label: 'OEM identifier (0xFD01)', requestHex: '2E FD 01 00', responseHex: '6E FD 01' },
      {
        label: 'any identifier, any value',
        requestHex: '2E ** **',
        responseHex: '6E 00 00',
        echoSpans: c_echoIdentifier,
      },
    ],
  },
  {
    sid: '2F',
    name: 'InputOutputControlByIdentifier',
    implemented: false,
    variants: [
      {
        label: 'returnControlToECU (0x00)',
        requestHex: '2F F1 90 00',
        responseHex: '6F F1 90 00',
      },
      { label: 'resetToDefault (0x01)', requestHex: '2F F1 90 01', responseHex: '6F F1 90 01' },
      {
        label: 'freezeCurrentState (0x02)',
        requestHex: '2F F1 90 02',
        responseHex: '6F F1 90 02',
      },
      {
        label: 'shortTermAdjustment (0x03)',
        requestHex: '2F F1 90 03 01',
        responseHex: '6F F1 90 03',
      },
    ],
  },
  {
    sid: '31',
    name: 'RoutineControl',
    implemented: true,
    variants: [
      { label: 'startRoutine (0x01)', requestHex: '31 01 F0 00', responseHex: '71 01 F0 00' },
      { label: 'stopRoutine (0x02)', requestHex: '31 02 F0 00', responseHex: '71 02 F0 00' },
      {
        label: 'requestRoutineResults (0x03)',
        requestHex: '31 03 F0 00',
        responseHex: '71 03 F0 00 00',
      },
      {
        label: 'any routine, start',
        requestHex: '31 01 ** **',
        responseHex: '71 01 00 00',
        echoSpans: [{ requestOffset: 2, length: 2, responseOffset: 2 }],
      },
    ],
  },
  {
    sid: '34',
    name: 'RequestDownload',
    implemented: false,
    variants: [
      {
        label: 'no compression, 4-byte address and size',
        requestHex: '34 00 44 00 00 00 00 00 00 01 00',
        responseHex: '74 20 04 00',
      },
    ],
  },
  {
    sid: '36',
    name: 'TransferData',
    implemented: false,
    variants: [
      { label: 'block 1', requestHex: '36 01', responseHex: '76 01' },
      {
        label: 'any block',
        requestHex: '36 **',
        responseHex: '76 00',
        echoSpans: [{ requestOffset: 1, length: 1, responseOffset: 1 }],
      },
    ],
  },
  {
    sid: '37',
    name: 'RequestTransferExit',
    implemented: false,
    variants: [{ label: 'exit', requestHex: '37', responseHex: '77' }],
  },
  {
    sid: '3E',
    name: 'TesterPresent',
    implemented: true,
    variants: [
      { label: 'zeroSubFunction (0x00)', requestHex: '3E 00', responseHex: '7E 00' },
      { label: 'response suppressed (0x80)', requestHex: '3E 80', responseHex: '7E 00' },
    ],
  },
  {
    sid: '85',
    name: 'ControlDTCSetting',
    implemented: false,
    variants: [
      { label: 'on (0x01)', requestHex: '85 01', responseHex: 'C5 01' },
      { label: 'off (0x02)', requestHex: '85 02', responseHex: 'C5 02' },
    ],
  },
  {
    sid: '87',
    name: 'LinkControl',
    implemented: false,
    variants: [
      {
        label: 'verifyModeTransitionWithFixedParameter (0x01)',
        requestHex: '87 01 01',
        responseHex: 'C7 01',
      },
      {
        label: 'verifyModeTransitionWithSpecificParameter (0x02)',
        requestHex: '87 02 00 7A 12',
        responseHex: 'C7 02',
      },
      { label: 'transitionMode (0x03)', requestHex: '87 03', responseHex: 'C7 03' },
    ],
  },
]

/** Common negative responses, offered so refusing a request is as easy as answering it. */
export const NEGATIVE_RESPONSES: { label: string; nrc: string }[] = [
  { label: '0x11 serviceNotSupported', nrc: '11' },
  { label: '0x12 sub-functionNotSupported', nrc: '12' },
  { label: '0x13 incorrectMessageLengthOrInvalidFormat', nrc: '13' },
  { label: '0x22 conditionsNotCorrect', nrc: '22' },
  { label: '0x24 requestSequenceError', nrc: '24' },
  { label: '0x31 requestOutOfRange', nrc: '31' },
  { label: '0x33 securityAccessDenied', nrc: '33' },
  { label: '0x35 invalidKey', nrc: '35' },
  { label: '0x72 generalProgrammingFailure', nrc: '72' },
  { label: '0x7F serviceNotSupportedInActiveSession', nrc: '7F' },
]

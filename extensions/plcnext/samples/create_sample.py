import zipfile
import os
from pathlib import Path

def create_sample_project():
    sample_dir = Path("extensions/plcnext/samples")
    sample_dir.mkdir(parents=True, exist_ok=True)
    sample_path = sample_dir / "SortingPlant_AXCF2152.pcwex"

    manifest_xml = """<?xml version="1.0" encoding="utf-8"?>
<Manifest VersionId="2024.0.2">
    <Target>AXC F 2152</Target>
    <Firmware>2024.0 LTS</Firmware>
    <Author>Phoenix Contact Automation</Author>
    <Date>2026-10-29</Date>
    <Guid>e6b947a1-8d23-4c9f-9f1e-7b3e0c4821a0</Guid>
    <Description>Automated High-Speed Sorting Cell with Axioline F I/O</Description>
</Manifest>"""

    structure_xml = """<?xml version="1.0" encoding="utf-8"?>
<Solution Name="SortingPlant_AXCF2152">
    <ExecutionAndSynchronizationManager>
        <Task Name="CyclicTask" CycleTime="T#20ms" Priority="50" Watchdog="T#50ms" Type="Cyclic">
            <Program Name="MainControl" InstanceName="MainControl_1" Order="1" />
            <Program Name="ConveyorSequence" InstanceName="ConveyorSequence_1" Order="2" />
        </Task>
        <Task Name="FastTask" CycleTime="T#5ms" Priority="20" Watchdog="T#15ms" Type="Cyclic">
            <Program Name="SensorFilter" InstanceName="SensorFilter_1" Order="1" />
        </Task>
    </ExecutionAndSynchronizationManager>
    <HardwareTopology>
        <Module Name="AXC F 2152" OrderNumber="2404267" Slot="0" Description="PLCnext Control CPU">
            <Channel Name="IN00" Index="0" Direction="Input" DataType="BOOL" Variable="bEmergencyStop" />
            <Channel Name="IN01" Index="1" Direction="Input" DataType="BOOL" Variable="bStartButton" />
            <Channel Name="OUT00" Index="0" Direction="Output" DataType="BOOL" Variable="bSystemReadyLed" />
        </Module>
        <Module Name="AXL F DI16/1 1H" OrderNumber="2688310" Slot="1" Description="16 Digital Inputs 24V DC">
            <Channel Name="DI_00" Index="0" Direction="Input" DataType="BOOL" Variable="bPartDetectInfeed" />
            <Channel Name="DI_01" Index="1" Direction="Input" DataType="BOOL" Variable="bPartDetectDiverter" />
            <Channel Name="DI_02" Index="2" Direction="Input" DataType="BOOL" Variable="bBinFullSensor" />
        </Module>
        <Module Name="AXL F DO16/1 1H" OrderNumber="2688349" Slot="2" Description="16 Digital Outputs 500mA">
            <Channel Name="DO_00" Index="0" Direction="Output" DataType="BOOL" Variable="bConveyorMotorRun" />
            <Channel Name="DO_01" Index="1" Direction="Output" DataType="BOOL" Variable="bPneumaticDiverterPiston" />
        </Module>
        <Module Name="AXL F AI4 I 1H" OrderNumber="2688491" Slot="3" Description="4 Analog Inputs 4-20mA">
            <Channel Name="AI_00" Index="0" Direction="Input" DataType="INT" Variable="rawLaserHeightSensor" />
        </Module>
    </HardwareTopology>
</Solution>"""

    main_st = """(* =========================================================================
   POU: MainControl
   Description: Top-level supervisor state machine for sorting cell
   ========================================================================= *)
PROGRAM MainControl
VAR
    fbMotor : FB_MotorDrive;
    rActualHeight_mm : REAL;
END_VAR

// Safety Interlock check
IF NOT bEmergencyStop THEN
    bSystemRunning := FALSE;
    bAlarmActive := TRUE;
    iState := 99; // Emergency Stop State
    RETURN;
END_IF;

// Main sequencer state machine
CASE iState OF
    0: // Initialization / Standby
        bAlarmActive := FALSE;
        IF bStartButton AND NOT bAlarmActive THEN
            bSystemRunning := TRUE;
            iState := 10;
        END_IF;

    10: // Conveyor Infeed Running
        fbMotor(
            bEnable := TRUE,
            rTargetSpeed := rNominalSpeed,
            bFaultAck := bResetFault
        );
        bConveyorMotorRun := fbMotor.bRunning;

        // Part arrives at laser measurement station
        IF bPartDetectInfeed THEN
            rActualHeight_mm := FC_ScaleAnalog(
                nRawInput := rawLaserHeightSensor,
                rRawMin := 0.0,
                rRawMax := 32767.0,
                rScaledMin := 0.0,
                rScaledMax := 250.0
            );
            iPartCount := iPartCount + 1;
            iState := 20;
        END_IF;

    20: // Sorting Decision
        IF rActualHeight_mm >= 120.0 THEN
            eLastSortClass := E_SortingMode#HeavyPackage;
            bPneumaticDiverterPiston := TRUE;
        ELSE
            eLastSortClass := E_SortingMode#StandardPackage;
            bPneumaticDiverterPiston := FALSE;
        END_IF;
        iState := 10; // Return to continuous feed

    99: // Fault / E-Stop
        fbMotor(bEnable := FALSE, rTargetSpeed := 0.0, bFaultAck := bResetFault);
        bConveyorMotorRun := FALSE;
        bPneumaticDiverterPiston := FALSE;
        IF bResetFault AND bEmergencyStop THEN
            iState := 0;
        END_IF;
END_CASE;
END_PROGRAM
"""

    main_var = """<?xml version="1.0" encoding="utf-8"?>
<Variables>
    <Variable Name="bStartButton" Type="BOOL" Usage="Input" Port="true"><Comment>Operator Panel Start</Comment></Variable>
    <Variable Name="bEmergencyStop" Type="BOOL" Usage="Input" Port="true"><Comment>Safety Relay Status</Comment></Variable>
    <Variable Name="bResetFault" Type="BOOL" Usage="Input" Port="true"><Comment>Fault Reset Pushbutton</Comment></Variable>
    <Variable Name="bPartDetectInfeed" Type="BOOL" Usage="Input" Port="true"><Comment>Infeed Photo-eye</Comment></Variable>
    <Variable Name="rawLaserHeightSensor" Type="INT" Usage="Input" Port="true"><Comment>Axioline 4-20mA Analog Input</Comment></Variable>
    <Variable Name="bConveyorMotorRun" Type="BOOL" Usage="Output" Port="true"><Comment>Contactor Q1 Coil</Comment></Variable>
    <Variable Name="bPneumaticDiverterPiston" Type="BOOL" Usage="Output" Port="true"><Comment>Solenoid Valve Y1</Comment></Variable>
    <Variable Name="bSystemRunning" Type="BOOL" Usage="Output" Port="true"><Comment>System Status Output</Comment></Variable>
    <Variable Name="bAlarmActive" Type="BOOL" Usage="Output" Port="true"><Comment>Active Fault Warning</Comment></Variable>
    <Variable Name="iPartCount" Type="DINT" Usage="Output" Retain="true" Port="true" InitialValue="0"><Comment>Total Parts Sorted</Comment></Variable>
    <Variable Name="iState" Type="INT" Usage="VAR" InitialValue="0"><Comment>Sequencer Step Index</Comment></Variable>
    <Variable Name="rNominalSpeed" Type="REAL" Usage="VAR" InitialValue="1500.0"><Comment>Conveyor RPM</Comment></Variable>
</Variables>"""

    fb_motor_st = """(* =========================================================================
   POU: FB_MotorDrive
   Description: Standard VFD driver with acceleration ramp and thermal watchdog
   ========================================================================= *)
FUNCTION_BLOCK FB_MotorDrive
VAR_INPUT
    bEnable : BOOL;
    rTargetSpeed : REAL;
    bFaultAck : BOOL;
END_VAR
VAR_OUTPUT
    bRunning : BOOL;
    rCurrentSpeed : REAL;
    bDriveFault : BOOL;
END_VAR
VAR
    rSpeedRamp : REAL := 0.0;
END_VAR

IF bFaultAck THEN
    bDriveFault := FALSE;
END_IF;

IF bEnable AND NOT bDriveFault THEN
    IF rCurrentSpeed < rTargetSpeed THEN
        rCurrentSpeed := rCurrentSpeed + 25.0; // Acceleration
        IF rCurrentSpeed > rTargetSpeed THEN
            rCurrentSpeed := rTargetSpeed;
        END_IF;
    ELSIF rCurrentSpeed > rTargetSpeed THEN
        rCurrentSpeed := rCurrentSpeed - 25.0; // Deceleration
    END_IF;
    bRunning := (rCurrentSpeed > 0.0);
ELSE
    rCurrentSpeed := 0.0;
    bRunning := FALSE;
END_IF;
END_FUNCTION_BLOCK
"""

    fb_motor_var = """<?xml version="1.0" encoding="utf-8"?>
<Variables>
    <Variable Name="bEnable" Type="BOOL" Usage="Input"><Comment>Drive enable request</Comment></Variable>
    <Variable Name="rTargetSpeed" Type="REAL" Usage="Input" InitialValue="0.0"><Comment>Velocity setpoint in RPM</Comment></Variable>
    <Variable Name="bFaultAck" Type="BOOL" Usage="Input"><Comment>Reset drive alarm</Comment></Variable>
    <Variable Name="bRunning" Type="BOOL" Usage="Output"><Comment>Drive is active and modulating</Comment></Variable>
    <Variable Name="rCurrentSpeed" Type="REAL" Usage="Output"><Comment>Actual measured RPM</Comment></Variable>
    <Variable Name="bDriveFault" Type="BOOL" Usage="Output"><Comment>Thermal or overload trip</Comment></Variable>
</Variables>"""

    fc_scale_st = """(* =========================================================================
   POU: FC_ScaleAnalog
   Description: Linear scaling function for analog process variables
   ========================================================================= *)
FUNCTION FC_ScaleAnalog : REAL
VAR_INPUT
    nRawInput : INT;
    rRawMin : REAL;
    rRawMax : REAL;
    rScaledMin : REAL;
    rScaledMax : REAL;
END_VAR
VAR
    rRaw : REAL;
END_VAR

rRaw := INT_TO_REAL(nRawInput);
IF (rRawMax - rRawMin) <> 0.0 THEN
    FC_ScaleAnalog := ((rRaw - rRawMin) / (rRawMax - rRawMin)) * (rScaledMax - rScaledMin) + rScaledMin;
ELSE
    FC_ScaleAnalog := rScaledMin;
END_IF;
END_FUNCTION
"""

    types_dt = """<?xml version="1.0" encoding="utf-8"?>
<DataTypes>
    <DataType Name="E_SortingMode" Comment="Sorting classification category">
        <Enum>
            <Value Name="StandardPackage" Value="0" />
            <Value Name="HeavyPackage" Value="1" />
            <Value Name="Hazardous" Value="2" />
            <Value Name="Rejected" Value="3" />
        </Enum>
    </DataType>
    <DataType Name="ST_ConveyorConfig" Comment="Belt line geometry and speed limits">
        <Struct>
            <Field Name="Length_mm" Type="REAL" InitialValue="4500.0" Comment="Total belt length" />
            <Field Name="MaxSpeed_rpm" Type="REAL" InitialValue="3000.0" Comment="VFD rated max speed" />
            <Field Name="AccelTime_s" Type="TIME" InitialValue="T#1s500ms" Comment="Ramp up duration" />
            <Field Name="AutoReverseOnJam" Type="BOOL" InitialValue="TRUE" Comment="Automatic un-jam cycle" />
        </Struct>
    </DataType>
</DataTypes>"""

    global_var = """<?xml version="1.0" encoding="utf-8"?>
<Variables>
    <Variable Name="g_eActiveMode" Type="E_SortingMode" Usage="Global" Port="true"><Comment>Global Operating Mode</Comment></Variable>
    <Variable Name="g_stConfig" Type="ST_ConveyorConfig" Usage="Global" Retain="true"><Comment>Plant Persistent Settings</Comment></Variable>
    <Variable Name="g_nGlobalAlarms" Type="DWORD" Usage="Global" Port="true"><Comment>Alarm bitmask for SCADA/HMI</Comment></Variable>
</Variables>"""

    with zipfile.ZipFile(sample_path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("manifest.xml", manifest_xml)
        z.writestr("structure.xml", structure_xml)
        z.writestr("content/MainControl~Code~0000.st", main_st)
        z.writestr("content/MainControl~Variables~0000.var", main_var)
        z.writestr("content/FB_MotorDrive~Code~0000.st", fb_motor_st)
        z.writestr("content/FB_MotorDrive~Variables~0000.var", fb_motor_var)
        z.writestr("content/FC_ScaleAnalog~Code~0000.st", fc_scale_st)
        z.writestr("content/Types~GeneratedD~0000.dt", types_dt)
        z.writestr("content/PROJECT~Variables~0000.var", global_var)

    print(f"Sample PLCnext project created: {sample_path}")

if __name__ == "__main__":
    create_sample_project()

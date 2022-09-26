package argoclient

import (
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"fmt"
	"regexp"
	"time"

	api "github.com/ag-computational-bio/bakta-web-api-go/bakta/web/api/proto/v1"
	"github.com/google/uuid"
	log "github.com/sirupsen/logrus"
)

type StatusHandler struct {
	wfStatus      map[string]WorkflowStatus
	wfInitialized map[string]WorkflowStatus
	argoClient    *ArgoClient
}

var regex = regexp.MustCompile("^(([A-Za-z0-9][-A-Za-z0-9_.]*)?[A-Za-z0-9])?$")

type WorkflowStatus struct {
	JobId, Name, Secret, Status, Message, FullName string
	Started, Updated                               time.Time
}

func NewStatusHandler(client *ArgoClient) *StatusHandler {

	return &StatusHandler{argoClient: client, wfStatus: make(map[string]WorkflowStatus), wfInitialized: make(map[string]WorkflowStatus)}
}

func (status *StatusHandler) Run() {

	go func() {
		for {
			status.UpdateStatus()
			time.Sleep(10 * time.Second)
		}
	}()

}

func (status *StatusHandler) UpdateStatus() {

	wfstats, err := status.argoClient.GetWorkflowStatus()
	if err != nil {
		log.Errorf("error in getting new status: %v", err.Error())
	} else {
		status.wfStatus = *wfstats
	}

}

func (status *StatusHandler) StartJob(jobId, secret string, config *api.JobConfig) error {
	var fullname string
	if requestedStatus, ok := status.wfInitialized[jobId]; ok {
		if requestedStatus.Secret == secret {
			confString, err := CreateBaktaConfString(config)
			if err != nil {
				return err
			}

			fullname, err = status.argoClient.SubmitBaktaWorkflow(requestedStatus.Name, requestedStatus.JobId, requestedStatus.Secret, confString)

			if err != nil {
				return err
			}

			delete(status.wfInitialized, jobId)
			requestedStatus.Status = "Pending"
			requestedStatus.FullName = fullname
			status.wfStatus[jobId] = requestedStatus

			return nil
		} else {
			return fmt.Errorf("wrong secret")
		}

	} else {
		return fmt.Errorf("job not found")
	}

}

func (status *StatusHandler) InitJob(name string) (jobID, secret string, err error) {
	jobID = uuid.New().String()
	secretID, err := generateSecret()
	if err != nil {
		log.Println(err.Error())
		return "", "", err
	}

	wfStatus := WorkflowStatus{
		Name:     name,
		JobId:    jobID,
		Secret:   secretID,
		Status:   "Init",
		Message:  "",
		FullName: "",
		Started:  time.Now(),
		Updated:  time.Now(),
	}

	status.wfInitialized[jobID] = wfStatus

	return jobID, secretID, nil
}

func (status *StatusHandler) GetJob(jobID, secret string) (wfstatus *WorkflowStatus, err error) {

	if requestedStatus, ok := status.wfStatus[jobID]; ok {
		if requestedStatus.Secret == secret {
			return &requestedStatus, nil
		} else {
			return nil, fmt.Errorf("wrong secret")
		}
	} else if requestedStatus2, ok2 := status.wfInitialized[jobID]; ok2 {
		if requestedStatus2.Secret == secret {
			return &requestedStatus2, nil
		} else {
			return nil, fmt.Errorf("wrong secret")
		}
	} else {
		return nil, fmt.Errorf("job not found")
	}
}

func (status *StatusHandler) DeleteJob(jobID, secret string) error {

	stat, err := status.GetJob(jobID, secret)
	if err != nil {
		return err
	}
	err = status.argoClient.DeleteWorkflow(stat.FullName)
	if err != nil {
		return err
	}
	delete(status.wfStatus, jobID)
	return nil
}

func (status *StatusHandler) ParseStatus(statusstring string) api.JobStatusEnum {

	switch statusstring {
	case "Init":
		return api.JobStatusEnum_INIT
	case "Pending":
		return api.JobStatusEnum_INIT
	case "Running":
		return api.JobStatusEnum_RUNNING
	case "Succeeded":
		return api.JobStatusEnum_SUCCESSFULL
	case "Failed":
		return api.JobStatusEnum_ERROR
	case "Error":
		return api.JobStatusEnum_ERROR
	default:
		return api.JobStatusEnum_ERROR
	}

}

func generateSecret() (string, error) {

	for {
		b := make([]byte, 50)
		_, err := rand.Read(b)
		if err != nil {
			log.Println(err.Error())
			return "", err
		}

		data := base64.StdEncoding.EncodeToString(b)
		secretSHA := sha256.Sum256([]byte(data))
		secretSHABase64 := base64.RawURLEncoding.EncodeToString(secretSHA[:])

		if regex.MatchString(secretSHABase64) {
			return secretSHABase64, nil
		}
	}
}

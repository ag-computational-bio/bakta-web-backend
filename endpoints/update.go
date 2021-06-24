package endpoints

import (
	"context"
	"log"
	"time"

	api "github.com/ag-computational-bio/bakta-web-api-go/bakta/web/api/proto/v1"
	"github.com/ag-computational-bio/bakta-web-backend/database"
	"github.com/ag-computational-bio/bakta-web-backend/monitor"
	"github.com/ag-computational-bio/bakta-web-backend/scheduler"
)

//BaktaUpdateAPI implements the endpoints of the bakta-web-api
type BaktaUpdateAPI struct {
	api.UnimplementedBaktaStatusUpdateServer
	dbHandler     *database.Handler
	scheduler     *scheduler.SimpleScheduler
	updateMonitor *monitor.SimpleMonitor
}

//UpdateStatus Updates the status of a running job
func (apiHandler *BaktaUpdateAPI) UpdateStatus(ctx context.Context, request *api.UpdateStatusRequest) (*api.Empty, error) {
	go func() {
		log.Println("Received job update")

		job, err := apiHandler.dbHandler.GetJob(request.GetJobID())
		if err != nil {
			log.Println(err.Error())
			return
		}

		if job.IsDeleted {
			return
		}

		status, err := apiHandler.updateMonitor.GetJobStatus(job.JobID)
		if err != nil {
			log.Println(err.Error())
			return
		}

		isDeleted := false

		repeats := 0

		for {
			repeats = repeats + 1
			status, err = apiHandler.updateMonitor.GetJobStatus(job.JobID)
			if err != nil {
				log.Println(err.Error())
				return
			}

			if status.Status != api.JobStatusEnum_RUNNING {
				break
			}

			if repeats > 15 {
				break
			}

			time.Sleep(500 * time.Millisecond)
		}

		if status.Status == api.JobStatusEnum_SUCCESSFULL || status.Status == api.JobStatusEnum_ERROR {
			if !job.IsDeleted {
				err = apiHandler.scheduler.DeleteJob(job.JobID)
				if err != nil {
					log.Println(err.Error())
					return
				}

				isDeleted = true
			}
		}

		err = apiHandler.dbHandler.UpdateStatus(request.GetJobID(), status.Status, status.ErrorMsg, isDeleted)
		if err != nil {
			log.Println(err.Error())
			return
		}

	}()

	return &api.Empty{}, nil
}
